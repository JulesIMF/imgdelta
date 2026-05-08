// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Compress pipeline: CompressPipeline runner (stages 1–7)

use std::path::Path;
use std::time::Instant;

use tracing::info;

use super::context::StageContext;
use super::stage::CompressStage;
use super::stages::{
    Cleanup, ComputePatches, DownloadBlobsForPatches, MatchRenamed, S3Lookup, UploadLazyBlobs,
};
use crate::compress::partitions::fs::draft::FsDraft;
use crate::Result;

/// Runs stages 1–7 of the compress pipeline in order.
///
/// Stage 8 ([`PackAndUploadArchive`]) is excluded here because it returns
/// `(PartitionContent, bool, u64)` rather than [`FsDraft`].  The entry-point
/// function [`compress_fs_partition`] calls stage 8 directly after this
/// pipeline completes.
///
/// [`PackAndUploadArchive`]: super::stages::PackAndUploadArchive
/// [`compress_fs_partition`]: super::compress_fs_partition
pub struct CompressPipeline {
    stages: Vec<Box<dyn CompressStage>>,
}

impl CompressPipeline {
    /// Construct the default 6-stage pipeline (stages 2–7 of 8).
    ///
    /// Order: S3Lookup → MatchRenamed → Cleanup →
    ///        UploadLazyBlobs → DownloadBlobsForPatches → ComputePatches.
    ///
    /// Stage 1 (Walkdir) is called directly before the pipeline because it
    /// requires `base_root`/`target_root` path arguments not available via
    /// the [`CompressStage`] trait.
    pub fn default_fs() -> Self {
        Self {
            stages: vec![
                Box::new(S3Lookup),
                Box::new(MatchRenamed::default()),
                Box::new(Cleanup),
                Box::new(UploadLazyBlobs),
                Box::new(DownloadBlobsForPatches),
                Box::new(ComputePatches),
            ],
        }
    }

    /// Run all stages in order, threading `initial` through each stage.
    ///
    /// Logs stage name, index, and elapsed time via `tracing::info!`.
    ///
    /// If `debug_dir` is `Some`, calls [`CompressStage::dump_debug`] after each
    /// stage and writes a snapshot to `<debug_dir>/<NN>_<name>.json`.
    pub async fn run(
        &self,
        ctx: &StageContext,
        initial: FsDraft,
        debug_dir: Option<&Path>,
    ) -> Result<FsDraft> {
        let mut draft = initial;
        let n = self.stages.len();

        for (i, stage) in self.stages.iter().enumerate() {
            let t0 = Instant::now();
            info!("[{}/{}] {}: starting", i + 1, n, stage.name());
            draft = stage.run(ctx, draft).await?;
            let elapsed = t0.elapsed().as_secs_f64();
            info!(
                "[{}/{}] {}: done in {:.2}s",
                i + 1,
                n,
                stage.name(),
                elapsed
            );

            if let Some(dir) = debug_dir {
                let path = dir.join(format!("{:02}_{}.json", i + 1, stage.name()));
                stage.dump_debug(&draft, &path)?;
            }
        }

        Ok(draft)
    }
}
