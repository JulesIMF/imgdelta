// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Compress pipeline: CompressStage trait

use std::path::Path;

use async_trait::async_trait;

use super::context::StageContext;
use crate::compress::FsDraft;
use crate::manifest::Record;
use crate::Result;

/// A single stage in the compress pipeline.
///
/// Each stage is a stateless struct that transforms [`FsDraft`] given the
/// immutable shared [`StageContext`].  Stages are designed to be independently
/// testable: a stage should depend only on the data it receives, not on any
/// external global state.
#[async_trait]
pub trait CompressStage: Send + Sync {
    /// Human-readable name for logging and debug output (e.g. `"walkdir"`).
    fn name(&self) -> &'static str;

    /// Execute the stage.
    ///
    /// Receives ownership of `draft`, transforms it, and returns it.  The
    /// pipeline runner calls stages in order, chaining the output of each as
    /// the input to the next.
    async fn run(&self, ctx: &StageContext, draft: FsDraft) -> Result<FsDraft>;

    /// Optionally dump a debug snapshot of `draft` to `path` (JSON).
    ///
    /// The default implementation is a no-op.  Stages may override this to
    /// serialize relevant fields for offline debugging.
    fn dump_debug(&self, _draft: &FsDraft, _path: &Path) -> Result<()> {
        Ok(())
    }
}

/// Serialize `records` as a pretty-printed JSON array to `path`.
///
/// Called by stages that implement [`CompressStage::dump_debug`].
pub fn dump_records_json(records: &[Record], path: &Path) -> Result<()> {
    use std::fs;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            crate::Error::Other(format!("create debug dir {}: {e}", parent.display()))
        })?;
    }
    let json = serde_json::to_string_pretty(records)
        .map_err(|e| crate::Error::Other(format!("serialize records: {e}")))?;
    fs::write(path, json).map_err(|e| {
        crate::Error::Other(format!("write debug snapshot {}: {e}", path.display()))
    })?;
    Ok(())
}
