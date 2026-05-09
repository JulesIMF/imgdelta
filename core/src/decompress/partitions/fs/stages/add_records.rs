// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Decompress stage 4 (m^A): new entity additions.

use crate::decompress::partitions::fs::context::DecompressContext;
use crate::decompress::partitions::fs::draft::DecompressDraft;
use crate::decompress::partitions::fs::stage::DecompressStage;
use crate::decompress::partitions::fs::stages::apply_records::run_phase;
use crate::manifest::Record;
use crate::Result;

/// Stage 4 — $m^A$: additions (old\_path = `None`, new\_path = `Some`).
///
/// Creates every entity that does not exist in the base image.  Mirrors
/// the formal model's final step:
///
/// ```text
/// … ∪ c^A
/// ```
pub struct AddRecords;

#[async_trait::async_trait]
impl DecompressStage for AddRecords {
    fn name(&self) -> &'static str {
        "add_records"
    }

    async fn run(
        &self,
        ctx: &DecompressContext,
        mut draft: DecompressDraft,
    ) -> Result<DecompressDraft> {
        let records: Vec<&Record> = ctx
            .records
            .iter()
            .filter(|r| r.old_path.is_none() && r.new_path.is_some())
            .collect();

        run_phase(
            &records,
            &ctx.base_root,
            &ctx.output_root,
            &ctx.patch_map,
            &draft.blob_cache,
            &ctx.router,
            ctx.workers,
            &mut draft.stats,
        )?;
        Ok(draft)
    }
}
