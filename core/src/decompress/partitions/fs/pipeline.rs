// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Decompress pipeline runner

use std::time::Instant;

use tracing::info;

use super::context::DecompressContext;
use super::draft::DecompressDraft;
use super::stage::DecompressStage;
use super::stages::{
    AddRecords, ChangeRecords, CopyUnchanged, DeleteRecords, DownloadBlobs, RenameRecords,
};
use crate::Result;

/// Runs all decompress pipeline stages in order.
///
/// Order:
///   `CopyUnchanged`
///   → `DownloadBlobs`
///   → `DeleteRecords`  (m^R)
///   → `RenameRecords`  (m^N)
///   → `ChangeRecords`  (m^C)
///   → `AddRecords`     (m^A)
///
/// Mirrors `decompress(a, m) = (((a \ a^R) \ a^N ∪ c^N) \ a^C ∪ c^C) ∪ c^A`.
pub struct DecompressPipeline {
    stages: Vec<Box<dyn DecompressStage>>,
}

impl DecompressPipeline {
    /// Construct the default pipeline matching the formal decompress model.
    pub fn default_fs() -> Self {
        Self {
            stages: vec![
                Box::new(CopyUnchanged),
                Box::new(DownloadBlobs),
                Box::new(DeleteRecords),
                Box::new(RenameRecords),
                Box::new(ChangeRecords),
                Box::new(AddRecords),
            ],
        }
    }

    /// Run all stages in order, threading `initial` through each stage.
    pub async fn run(
        &self,
        ctx: &DecompressContext,
        initial: DecompressDraft,
    ) -> Result<DecompressDraft> {
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
        }

        Ok(draft)
    }
}
