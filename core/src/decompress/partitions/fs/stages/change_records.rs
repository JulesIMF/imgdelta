// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Decompress stage 3 (m^C): in-place changes with lazy patch application.

use crate::decompress::partitions::fs::context::DecompressContext;
use crate::decompress::partitions::fs::draft::DecompressDraft;
use crate::decompress::partitions::fs::stage::DecompressStage;
use crate::decompress::partitions::fs::stages::apply_records::run_phase;
use crate::manifest::Record;
use crate::Result;

/// Stage 3 — $m^C$: in-place changes (old\_path = new\_path, both `Some`).
///
/// Patches each entity that stays at the same path.  Mirrors the formal
/// model step:
///
/// ```text
/// … \ a^C ∪ c^C …
/// ```
pub struct ChangeRecords;

#[async_trait::async_trait]
impl DecompressStage for ChangeRecords {
    fn name(&self) -> &'static str {
        "change_records"
    }

    async fn run(
        &self,
        ctx: &DecompressContext,
        mut draft: DecompressDraft,
    ) -> Result<DecompressDraft> {
        let records: Vec<&Record> = ctx
            .records
            .iter()
            .filter(|r| matches!((&r.old_path, &r.new_path), (Some(o), Some(n)) if o == n))
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
