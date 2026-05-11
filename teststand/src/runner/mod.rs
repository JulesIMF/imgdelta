// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Jules IMF
//
// image-delta — incremental disk-image compression toolkit
// Experiment orchestration: base-relative compression runner.

pub mod executor;
pub mod progress;
pub mod queue;

use std::sync::Arc;
use tracing::{error, info};

use crate::{
    config::families::{FamiliesConfig, FamilySpec},
    config::TeststandConfig,
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

    fn log(&self, experiment_id: &str, level: &str, message: impl Into<String>) {
        let _ = self.progress_tx.send(ProgressEvent::Log {
            experiment_id: Some(experiment_id.to_owned()),
            run_id: None,
            level: level.to_owned(),
            message: message.into(),
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
        self.log(
            exp_id,
            "info",
            format!("Starting experiment '{}'", spec.name),
        );

        let result = self.run_base_relative(exp_id, spec, family).await;

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
            let all = &family.images;
            let ids_to_evict: Vec<String> = if let Some(filter) = &spec.images {
                filter.clone()
            } else {
                all.iter().map(|i| i.id.clone()).collect()
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

    /// Base-relative runner: images[0] is the base; images[1..] are all targets.
    /// For each (target, workers, run_idx) triple, compress target against base.
    async fn run_base_relative(
        &self,
        exp_id: &str,
        spec: &crate::config::ExperimentSpec,
        family: &FamilySpec,
    ) -> Result<()> {
        let all_images = &family.images;

        // Resolve image list (filter or use all).
        let images: Vec<&crate::config::ImageSpec> = if let Some(ids) = &spec.images {
            ids.iter()
                .filter_map(|id| all_images.iter().find(|i| &i.id == id))
                .collect()
        } else {
            all_images.iter().collect()
        };

        if images.len() < 2 {
            return Err(crate::error::Error::Config(
                "need at least 2 images: images[0] is base, images[1..] are targets".into(),
            ));
        }

        let base_spec = images[0];
        let targets = &images[1..];

        self.log(
            exp_id,
            "info",
            format!(
                "base = {}, targets = [{}], workers = {:?}, runs = {}",
                base_spec.id,
                targets
                    .iter()
                    .map(|t| t.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                spec.workers,
                spec.runs_per_pair,
            ),
        );

        // Download base image once.
        let base_path = self.image_manager.ensure(&base_spec.id).await?;

        for workers in &spec.workers {
            let workers = *workers;
            for (target_idx, target_spec) in targets.iter().enumerate() {
                let target_path = self.image_manager.ensure(&target_spec.id).await?;

                for run_idx in 0..spec.runs_per_pair {
                    let run_id = db::new_id();
                    let run = Run {
                        id: run_id.clone(),
                        experiment_id: exp_id.to_owned(),
                        run_index: (target_idx * spec.runs_per_pair + run_idx) as i64,
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

                    self.log(
                        exp_id,
                        "info",
                        format!(
                            "compressing {} → {} (workers={}, run {}/{})",
                            base_spec.id,
                            target_spec.id,
                            workers,
                            run_idx + 1,
                            spec.runs_per_pair,
                        ),
                    );

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
                                target_qcow2_bytes: Some(pr.target_file_bytes as i64),
                                cstar: Some(cstar),
                            };
                            db::insert_result(&self.db, &res).await?;
                            self.log(
                                exp_id,
                                "info",
                                format!(
                                    "done: {} archive={:.1}MB target={:.1}MB C*={:.4}",
                                    pr.image_id,
                                    pr.archive_bytes as f64 / 1e6,
                                    pr.target_file_bytes as f64 / 1e6,
                                    cstar,
                                ),
                            );
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
                            self.log(exp_id, "error", format!("compress failed: {msg}"));
                            db::update_run_done(&self.db, &run_id, Some(&msg)).await?;
                            db::append_log(&self.db, &run_id, "error", &msg).await?;
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// C* = archive_bytes / target_bytes
/// Measures how large the delta is relative to the target image.
/// 0.0 means perfect (zero delta); 1.0 means delta is same size as target.
fn compute_cstar(pr: &executor::PairResult) -> f64 {
    if pr.target_file_bytes == 0 {
        return 1.0;
    }
    pr.archive_bytes as f64 / pr.target_file_bytes as f64
}
