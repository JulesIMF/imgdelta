// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Decompress pipeline: DecompressStage trait

use async_trait::async_trait;

use crate::decompress::partitions::fs::context::DecompressContext;
use crate::decompress::partitions::fs::draft::DecompressDraft;
use crate::Result;

// ── DecompressStage ───────────────────────────────────────────────────────────

/// One stage in the decompress pipeline.
///
/// Implementations receive the current [`DecompressDraft`] (mutable
/// inter-stage state) and the immutable [`DecompressContext`], and
/// return an updated draft.
#[async_trait]
pub trait DecompressStage: Send + Sync {
    /// Short identifier used in log messages.
    fn name(&self) -> &'static str;

    /// Execute this stage.
    async fn run(&self, ctx: &DecompressContext, draft: DecompressDraft)
        -> Result<DecompressDraft>;
}
