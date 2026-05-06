// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Compress pipeline: Stage 4 — cleanup

use async_trait::async_trait;

use crate::compress::context::StageContext;
use crate::compress::stage::CompressStage;
use crate::compress::FsDraft;
use crate::Result;

// ── Stage struct ──────────────────────────────────────────────────────────────

/// Stage 4: finalise deletion records.
///
/// After rename matching, any remaining `new_path = None` records are true
/// deletions.  Their `data`, `patch`, and `metadata` fields are cleared — the
/// decompressor only needs `old_path` to know what to remove.
pub struct Cleanup;

#[async_trait]
impl CompressStage for Cleanup {
    fn name(&self) -> &'static str {
        "cleanup"
    }

    async fn run(&self, _ctx: &StageContext, draft: FsDraft) -> Result<FsDraft> {
        Ok(cleanup_fn(draft))
    }
}

// ── Implementation ────────────────────────────────────────────────────────────

pub fn cleanup_fn(mut draft: FsDraft) -> FsDraft {
    for record in &mut draft.records {
        if record.new_path.is_none() {
            record.data = None;
            record.patch = None;
            record.metadata = None;
        }
    }
    draft
}
