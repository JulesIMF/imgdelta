// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Decompress stage 1 (m^R): acknowledge deletions.

use tracing::debug;

use crate::decompress::partitions::fs::context::DecompressContext;
use crate::decompress::partitions::fs::draft::DecompressDraft;
use crate::decompress::partitions::fs::stage::DecompressStage;
use crate::Result;

/// Stage 1 — $m^R$: deletions.
///
/// The output partition is created fresh (`mkfs`), so deleted entries are
/// never written to it.  This stage acknowledges them explicitly to mirror
/// the formal model's first step:
///
/// ```text
/// decompress(a, m) = (((a \ a^R) \ a^N ∪ c^N) \ a^C ∪ c^C) ∪ c^A
/// ```
pub struct DeleteRecords;

#[async_trait::async_trait]
impl DecompressStage for DeleteRecords {
    fn name(&self) -> &'static str {
        "delete_records"
    }

    async fn run(
        &self,
        ctx: &DecompressContext,
        draft: DecompressDraft,
    ) -> Result<DecompressDraft> {
        for record in ctx.records.iter() {
            if record.old_path.is_some() && record.new_path.is_none() {
                debug!(
                    old_path = ?record.old_path,
                    "phase 1 (delete) — not present in fresh output"
                );
            }
        }
        Ok(draft)
    }
}
