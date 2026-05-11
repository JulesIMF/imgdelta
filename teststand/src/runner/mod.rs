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
        queue: Queue,
        config: Arc<TeststandConfig>,
        db: Db,
        families: Arc<FamiliesConfig>,
        image_manager: Arc<ImageManager>,
        progress_tx: ProgressTx,
        notify: Arc<NotifyManager>,
    ) -> Arc<Self> {
        Arc::new(Self {
            queue,
            config,
            db,
            families,
            image_manager,
            progress_tx,
            notify,
        })
    }

    /// Queue reference for inspecting pending items.
    pub fn queue(&self) -> &Queue {
        &self.queue
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

    fn log(
        &self,
        experiment_id: &str,
        run_id: Option<&str>,
        level: &str,
        message: impl Into<String>,
    ) {
        let msg = message.into();
        let _ = self.progress_tx.send(ProgressEvent::Log {
            experiment_id: Some(experiment_id.to_owned()),
            run_id: run_id.map(str::to_owned),
            level: level.to_owned(),
            message: msg.clone(),
        });
        // Persist to DB so historical log fetch works after the experiment ends.
        let db = self.db.clone();
        let exp_id = experiment_id.to_owned();
        let rid = run_id.map(str::to_owned);
        let lvl = level.to_owned();
        tokio::spawn(async move {
            if let Err(e) =
                db::append_experiment_log(&db, &exp_id, rid.as_deref(), &lvl, &msg).await
            {
                tracing::warn!(err = %e, "failed to persist experiment log");
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
        self.log(
            exp_id,
            None,
            "info",
            format!("Starting experiment '{}'", spec.name),
        );

        // Start notification.
        {
            let family_name = family.name.clone();
            let target_count = family
                .images
                .iter()
                .filter(|img| {
                    spec.images
                        .as_ref()
                        .map(|f| f.contains(&img.id))
                        .unwrap_or(true)
                })
                .count();
            let msg = format!(
                "\u{1F680} <b>Experiment '{}' started</b>\nFamily: <code>{}</code> \u{2022} {} images \u{2022} workers: {:?}",
                spec.name,
                family_name,
                target_count,
                spec.workers,
            );
            self.notify.send(&msg).await;
        }

        let started_at = std::time::Instant::now();
        let result = self.run_base_relative(exp_id, spec, family).await;

        match &result {
            Ok(()) => {
                db::update_experiment_status(&self.db, exp_id, "done").await?;
                let _ = self.progress_tx.send(ProgressEvent::ExperimentFinished {
                    experiment_id: exp_id.clone(),
                    status: "done".into(),
                });
                let msg = self
                    .build_completion_msg(
                        exp_id,
                        &spec.name,
                        "done",
                        started_at.elapsed().as_secs_f64(),
                    )
                    .await;
                self.notify.send(&msg).await;
            }
            Err(e) => {
                error!(err = %e, "experiment error");
                db::update_experiment_status(&self.db, exp_id, "error").await?;
                let _ = self.progress_tx.send(ProgressEvent::ExperimentFinished {
                    experiment_id: exp_id.clone(),
                    status: "error".into(),
                });
                let msg = self
                    .build_completion_msg(
                        exp_id,
                        &spec.name,
                        "error",
                        started_at.elapsed().as_secs_f64(),
                    )
                    .await;
                self.notify.send(&msg).await;
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

    /// Build a rich HTML completion summary from DB results.
    async fn build_completion_msg(
        &self,
        exp_id: &str,
        name: &str,
        status: &str,
        elapsed_secs: f64,
    ) -> String {
        let results = db::get_results_for_experiment(&self.db, exp_id)
            .await
            .unwrap_or_default();

        let mut seen_target: std::collections::HashSet<String> = Default::default();
        let mut total_target_bytes: f64 = 0.0;
        let mut total_archive_bytes: f64 = 0.0;
        let mut storage_peak: f64 = 0.0;
        let mut c_sum: f64 = 0.0;
        let mut cstar_sum: f64 = 0.0;
        let mut c_n: usize = 0;
        let mut cstar_n: usize = 0;

        for r in &results {
            if seen_target.insert(r.image_id.clone()) {
                total_target_bytes += r.target_qcow2_bytes.unwrap_or(0) as f64;
                total_archive_bytes += r.archive_bytes.unwrap_or(0) as f64;
            }
            let sb = r.storage_bytes.unwrap_or(0) as f64;
            if sb > storage_peak {
                storage_peak = sb;
            }
            if let Some(c) = r.c {
                c_sum += c;
                c_n += 1;
            }
            if let Some(cs) = r.cstar {
                cstar_sum += cs;
                cstar_n += 1;
            }
        }

        let c_agg = if c_n > 0 { c_sum / c_n as f64 } else { 0.0 };
        let cstar_agg = if cstar_n > 0 {
            cstar_sum / cstar_n as f64
        } else {
            0.0
        };
        const MB: f64 = 1_048_576.0;

        crate::notify::fmt_completion_summary(
            name,
            status,
            elapsed_secs,
            results.len(),
            total_target_bytes / MB,
            total_archive_bytes / MB,
            storage_peak / MB,
            c_agg,
            cstar_agg,
        )
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

        // Per-experiment isolated storage: experiments/{exp_id}/storage/
        let storage_dir = self.config.experiment_storage_dir(exp_id);
        tokio::fs::create_dir_all(&storage_dir)
            .await
            .map_err(crate::error::Error::Io)?;

        let runs_total = spec.runs_per_pair as i64;
        let total_pairs = spec.workers.len() * targets.len() * spec.runs_per_pair;
        let mut done_pairs = 0u32;
        let started_at = std::time::Instant::now();

        self.log(
            exp_id,
            None,
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
                        Some(&run_id),
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
                        &storage_dir,
                        workers,
                        pt,
                        true,
                        &target_spec.format,
                    )
                    .await;

                    // Measure total storage dir after every compress call.
                    let storage_bytes = executor::dir_size_bytes(&storage_dir).unwrap_or(0) as i64;

                    match pair_result {
                        Ok(pr) => {
                            let c = compute_c(&pr);
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
                                decompress_stats_json: pr
                                    .decompress_stats
                                    .as_ref()
                                    .and_then(|d| serde_json::to_string(d).ok()),
                                timing_json,
                                archive_bytes: Some(pr.archive_bytes as i64),
                                base_qcow2_bytes: Some(pr.base_file_bytes as i64),
                                target_qcow2_bytes: Some(pr.target_file_bytes as i64),
                                storage_bytes: Some(storage_bytes),
                                cstar: Some(cstar),
                                c: Some(c),
                                workers: workers as i64,
                                run_repetition: run_idx as i64,
                                runs_total,
                            };
                            db::insert_result(&self.db, &res).await?;
                            self.log(
                                exp_id,
                                Some(&run_id),
                                "info",
                                format!(
                                    "done: {} archive={:.1}MB storage={:.1}MB C={:.2} C*={:.2}",
                                    pr.image_id,
                                    pr.archive_bytes as f64 / 1e6,
                                    storage_bytes as f64 / 1e6,
                                    c,
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
                                c,
                            });
                            done_pairs += 1;
                            let _ = self.progress_tx.send(ProgressEvent::ExperimentProgress {
                                experiment_id: exp_id.to_owned(),
                                done: done_pairs,
                                total: total_pairs as u32,
                                elapsed_secs: started_at.elapsed().as_secs_f64(),
                            });
                            db::update_run_done(&self.db, &run_id, None).await?;
                        }
                        Err(e) => {
                            let msg = e.to_string();
                            self.log(
                                exp_id,
                                Some(&run_id),
                                "error",
                                format!("compress failed: {msg}"),
                            );
                            done_pairs += 1;
                            let _ = self.progress_tx.send(ProgressEvent::ExperimentProgress {
                                experiment_id: exp_id.to_owned(),
                                done: done_pairs,
                                total: total_pairs as u32,
                                elapsed_secs: started_at.elapsed().as_secs_f64(),
                            });
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

/// C = target_bytes / archive_bytes
/// How many times larger the target is vs the delta archive.
/// Higher is better (e.g. C=11 means the delta is 11x smaller than the target).
fn compute_c(pr: &executor::PairResult) -> f64 {
    if pr.archive_bytes == 0 {
        return 1.0;
    }
    pr.target_file_bytes as f64 / pr.archive_bytes as f64
}

/// C* = (base_bytes + target_bytes) / (base_bytes + archive_bytes)
/// Storage efficiency including the shared base: how many times more data we
/// would store without delta compression vs with it.
fn compute_cstar(pr: &executor::PairResult) -> f64 {
    let denom = pr.base_file_bytes + pr.archive_bytes;
    if denom == 0 {
        return 1.0;
    }
    (pr.base_file_bytes + pr.target_file_bytes) as f64 / denom as f64
}
