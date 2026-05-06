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
use super::stages::{ApplyRecords, CopyUnchanged, ExtractArchive};
use crate::Result;

/// Runs stages 1–3 of the decompress pipeline in order.
///
/// Order: ExtractArchive → CopyUnchanged → ApplyRecords.
pub struct DecompressPipeline {
    stages: Vec<Box<dyn DecompressStage>>,
}

impl DecompressPipeline {
    /// Construct the default 3-stage pipeline.
    pub fn default_fs() -> Self {
        Self {
            stages: vec![
                Box::new(ExtractArchive),
                Box::new(CopyUnchanged),
                Box::new(ApplyRecords),
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
