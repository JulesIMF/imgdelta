// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Jules IMF
//
// image-delta — incremental disk-image compression toolkit
// Experiment orchestration: chain and scalability runners.

pub mod executor;
pub mod progress;
pub mod queue;

use std::sync::Arc;
use tracing::{error, info};

use crate::{
    config::families::{FamiliesConfig, FamilySpec},
    config::{experiment::ExperimentKind, TeststandConfig},
    db::{self, Db, ExperimentResult, Run},
    error::Result,
    image_manager::ImageManager,
    notify::NotifyManager,
};

use executor::compress_pair;
use progress::{ProgressEvent, ProgressTx};
use queue::{Queue, QueueItem};

pub struct Runner {
    pub queue: Queue,
    config: Arc<TeststandConfig>,
    db: Db,
    families: Arc<FamiliesConfig>,
    image_manager: Arc<ImageManager>,
    progress_tx: ProgressTx,
    notify: Arc<NotifyManager>,
}

impl Runner {
    pub fn new(
        config: Arc<TeststandConfig>,
        db: Db,
        families: Arc<FamiliesConfig>,
        image_manager: Arc<ImageManager>,
        progress_tx: ProgressTx,
        notify: Arc<NotifyManager>,
    ) -> Arc<Self> {
        Arc::new(Self {
            queue: Queue::new(),
            config,
            db,
            families,
            image_manager,
            progress_tx,
            notify,
        })
    }

    /// Spawn the background worker loop.
    pub fn start(self: Arc<Self>) {
        let runner = Arc::clone(&self);
        tokio::spawn(async move {
            loop {
                let item = runner.queue.pop_wait().await;
                info!(experiment_id = %item.experiment_id, "starting experiment");
                if let Err(e) = runner.run_experiment(item).await {
                    error!(err = %e, "experiment failed");
                }
            }
        });
    }

    async fn run_experiment(&self, item: QueueItem) -> Result<()> {
        let spec = &item.spec;
        let exp_id = &item.experiment_id;

        let family = self
            .families
            .families
            .iter()
            .find(|f| f.name == spec.family)
            .ok_or_else(|| {
                crate::error::Error::Config(format!("unknown family: {}", spec.family))
            })?;

        db::update_experiment_status(&self.db, exp_id, "running").await?;
        let _ = self.progress_tx.send(ProgressEvent::Log {
            run_id: None,
            level: "info".into(),
            message: format!("Starting experiment {} ({})", spec.name, spec.kind.as_str()),
        });

        let result = match spec.kind {
            ExperimentKind::Chain => self.run_chain(exp_id, spec, family).await,
            ExperimentKind::Scalability => self.run_scalability(exp_id, spec, family).await,
        };

        match &result {
            Ok(()) => {
                db::update_experiment_status(&self.db, exp_id, "done").await?;
                let _ = self.progress_tx.send(ProgressEvent::ExperimentFinished {
                    experiment_id: exp_id.clone(),
                    status: "done".into(),
                });
                self.notify
                    .send(&format!("Experiment '{}' completed.", spec.name))
                    .await;
            }
            Err(e) => {
                error!(err = %e, "experiment error");
                db::update_experiment_status(&self.db, exp_id, "error").await?;
                let _ = self.progress_tx.send(ProgressEvent::ExperimentFinished {
                    experiment_id: exp_id.clone(),
                    status: "error".into(),
                });
                self.notify
                    .send(&format!("Experiment '{}' FAILED: {}", spec.name, e))
                    .await;
            }
        }

        // Evict downloaded images to free disk space (unless keep_images = true).
        if !spec.keep_images.unwrap_or(false) {
            let ids_to_evict: Vec<String> = match spec.kind {
                ExperimentKind::Chain => {
                    let all = &family.images;
                    if let Some(filter) = &spec.images {
                        filter.clone()
                    } else {
                        all.iter().map(|i| i.id.clone()).collect()
                    }
                }
                ExperimentKind::Scalability => {
                    let mut ids = Vec::new();
                    if let Some(id) = &spec.base_image_id {
                        ids.push(id.clone());
                    }
                    if let Some(id) = &spec.target_image_id {
                        ids.push(id.clone());
                    }
                    ids
                }
            };
            for id in &ids_to_evict {
                if let Err(e) = self.image_manager.evict(id).await {
                    tracing::warn!(image_id = %id, err = %e, "evict failed (ignored)");
                } else {
                    tracing::info!(image_id = %id, "image evicted after experiment");
                }
            }
        }

        result
    }

    async fn run_chain(
        &self,
        exp_id: &str,
        spec: &crate::config::ExperimentSpec,
        family: &FamilySpec,
    ) -> Result<()> {
        // If spec.images is provided, pick only those IDs (in order); else use all.
        let all_images = &family.images;
        let filtered: Vec<&crate::config::ImageSpec> = if let Some(ids) = &spec.images {
            ids.iter()
                .filter_map(|id| all_images.iter().find(|i| &i.id == id))
                .collect()
        } else {
            all_images.iter().collect()
        };
        if filtered.len() < 2 {
            return Err(crate::error::Error::Config(
                "chain needs at least 2 images (check 'images' filter)".into(),
            ));
        }
        let images = filtered;

        for workers in &spec.workers {
            let workers = *workers;
            for (pair_idx, window) in images.windows(2).enumerate() {
                let base_spec = window[0];
                let target_spec = window[1];

                for run_idx in 0..spec.runs_per_pair {
                    let run_id = db::new_id();
                    let run = Run {
                        id: run_id.clone(),
                        experiment_id: exp_id.to_owned(),
                        run_index: (pair_idx * spec.runs_per_pair + run_idx) as i64,
                        workers: workers as i64,
                        phase: "compress".into(),
                        status: "pending".into(),
                        started_at: None,
                        finished_at: None,
                        error: None,
                    };
                    db::insert_run(&self.db, &run).await?;
                    db::update_run_started(&self.db, &run_id).await?;

                    let _ = self.progress_tx.send(ProgressEvent::RunStarted {
                        experiment_id: exp_id.to_owned(),
                        run_id: run_id.clone(),
                        workers,
                        phase: "compress".into(),
                    });

                    // Ensure both images are downloaded
                    let base_path = self.image_manager.ensure(&base_spec.id).await?;
                    let target_path = self.image_manager.ensure(&target_spec.id).await?;

                    let pt = spec
                        .passthrough_threshold
                        .unwrap_or(self.config.compressor.passthrough_threshold);

                    let pair_result = compress_pair(
                        &target_spec.id,
                        Some(&base_spec.id),
                        &target_path,
                        &base_path,
                        &self.config.storage_dir(),
                        workers,
                        pt,
                        false,
                        &target_spec.format,
                    )
                    .await;

                    match pair_result {
                        Ok(pr) => {
                            let cstar = compute_cstar(&pr);
                            let timing_json = pr
                                .compress_stats
                                .stage_timings
                                .as_ref()
                                .and_then(|t| serde_json::to_string(t).ok());
                            let res = ExperimentResult {
                                id: db::new_id(),
                                run_id: run_id.clone(),
                                image_id: pr.image_id.clone(),
                                base_image_id: pr.base_image_id.clone(),
                                compress_stats_json: serde_json::to_string(&pr.compress_stats).ok(),
                                decompress_stats_json: None,
                                timing_json,
                                archive_bytes: Some(pr.archive_bytes as i64),
                                base_qcow2_bytes: Some(pr.base_file_bytes as i64),
                                cstar: Some(cstar),
                            };
                            db::insert_result(&self.db, &res).await?;
                            let _ = self.progress_tx.send(ProgressEvent::ImageDone {
                                run_id: run_id.clone(),
                                image_id: pr.image_id,
                                base_image_id: pr.base_image_id,
                                compress_ms: (pr.compress_stats.elapsed_secs * 1000.0) as u64,
                                archive_bytes: pr.archive_bytes,
                                cstar,
                            });
                            db::update_run_done(&self.db, &run_id, None).await?;
                        }
                        Err(e) => {
                            let msg = e.to_string();
                            db::update_run_done(&self.db, &run_id, Some(&msg)).await?;
                            db::append_log(&self.db, &run_id, "error", &msg).await?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    async fn run_scalability(
        &self,
        exp_id: &str,
        spec: &crate::config::ExperimentSpec,
        family: &FamilySpec,
    ) -> Result<()> {
        let base_id = spec.base_image_id.as_deref().ok_or_else(|| {
            crate::error::Error::Config("Scalability experiment needs base_image_id".into())
        })?;
        let target_id = spec.target_image_id.as_deref().ok_or_else(|| {
            crate::error::Error::Config("Scalability experiment needs target_image_id".into())
        })?;

        let _base_spec = family
            .images
            .iter()
            .find(|i| i.id == base_id)
            .ok_or_else(|| {
                crate::error::Error::Config(format!("base image {base_id} not in family"))
            })?;
        let target_spec = family
            .images
            .iter()
            .find(|i| i.id == target_id)
            .ok_or_else(|| {
                crate::error::Error::Config(format!("target image {target_id} not in family"))
            })?;

        let base_path = self.image_manager.ensure(base_id).await?;
        let target_path = self.image_manager.ensure(target_id).await?;

        for workers in &spec.workers {
            let workers = *workers;
            for run_idx in 0..spec.runs_per_pair {
                let run_id = db::new_id();
                let run = Run {
                    id: run_id.clone(),
                    experiment_id: exp_id.to_owned(),
                    run_index: run_idx as i64,
                    workers: workers as i64,
                    phase: "compress".into(),
                    status: "pending".into(),
                    started_at: None,
                    finished_at: None,
                    error: None,
                };
                db::insert_run(&self.db, &run).await?;
                db::update_run_started(&self.db, &run_id).await?;

                let _ = self.progress_tx.send(ProgressEvent::RunStarted {
                    experiment_id: exp_id.to_owned(),
                    run_id: run_id.clone(),
                    workers,
                    phase: "compress".into(),
                });

                let pt = spec
                    .passthrough_threshold
                    .unwrap_or(self.config.compressor.passthrough_threshold);

                let pair_result = compress_pair(
                    target_id,
                    Some(base_id),
                    &target_path,
                    &base_path,
                    &self.config.storage_dir(),
                    workers,
                    pt,
                    false,
                    &target_spec.format,
                )
                .await;

                match pair_result {
                    Ok(pr) => {
                        let cstar = compute_cstar(&pr);
                        let timing_json = pr
                            .compress_stats
                            .stage_timings
                            .as_ref()
                            .and_then(|t| serde_json::to_string(t).ok());
                        let res = ExperimentResult {
                            id: db::new_id(),
                            run_id: run_id.clone(),
                            image_id: pr.image_id.clone(),
                            base_image_id: pr.base_image_id.clone(),
                            compress_stats_json: serde_json::to_string(&pr.compress_stats).ok(),
                            decompress_stats_json: None,
                            timing_json,
                            archive_bytes: Some(pr.archive_bytes as i64),
                            base_qcow2_bytes: Some(pr.base_file_bytes as i64),
                            cstar: Some(cstar),
                        };
                        db::insert_result(&self.db, &res).await?;
                        let _ = self.progress_tx.send(ProgressEvent::ImageDone {
                            run_id: run_id.clone(),
                            image_id: pr.image_id,
                            base_image_id: pr.base_image_id,
                            compress_ms: (pr.compress_stats.elapsed_secs * 1000.0) as u64,
                            archive_bytes: pr.archive_bytes,
                            cstar,
                        });
                        db::update_run_done(&self.db, &run_id, None).await?;
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        db::update_run_done(&self.db, &run_id, Some(&msg)).await?;
                        db::append_log(&self.db, &run_id, "error", &msg).await?;
                    }
                }
            }
        }
        Ok(())
    }
}

fn compute_cstar(pr: &executor::PairResult) -> f64 {
    // C*(n) = sum_qcow2 / (base_qcow2 + archive_size)
    // Here we approximate: numerator = base_dir_bytes + archive_bytes, denom = base_dir_bytes + archive_bytes
    // For a proper chain value the caller accumulates across the chain.
    // Per-pair value: (base + target raw) / (base + archive)
    let num = (pr.base_file_bytes + pr.compress_stats.total_stored_bytes) as f64;
    let den = (pr.base_file_bytes + pr.archive_bytes) as f64;
    if den == 0.0 {
        1.0
    } else {
        num / den
    }
}
