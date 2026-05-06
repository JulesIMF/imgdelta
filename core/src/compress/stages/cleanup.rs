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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{Data, DataRef, EntryType, Metadata, Patch, Record};

    #[test]
    fn test_cleanup_clears_deletion_records() {
        let mut draft = FsDraft::default();
        draft.records.push(Record {
            old_path: Some("etc/removed.conf".into()),
            new_path: None,
            entry_type: EntryType::File,
            size: 512,
            data: Some(Data::OriginalFile("/mnt/base/etc/removed.conf".into())),
            patch: None,
            metadata: Some(Metadata {
                mode: Some(0o644),
                ..Default::default()
            }),
        });

        let draft = cleanup_fn(draft);

        let r = &draft.records[0];
        assert!(r.data.is_none(), "data should be cleared");
        assert!(r.patch.is_none(), "patch should be cleared");
        assert!(r.metadata.is_none(), "metadata should be cleared");
    }

    #[test]
    fn test_cleanup_does_not_touch_non_deletions() {
        let mut draft = FsDraft::default();
        draft.records.push(Record {
            old_path: Some("etc/changed.conf".into()),
            new_path: Some("etc/changed.conf".into()),
            entry_type: EntryType::File,
            size: 512,
            data: None,
            patch: Some(Patch::Lazy {
                old_data: DataRef::FilePath("/mnt/base/etc/changed.conf".into()),
                new_data: DataRef::FilePath("/mnt/target/etc/changed.conf".into()),
            }),
            metadata: None,
        });

        let draft = cleanup_fn(draft);

        assert!(
            matches!(draft.records[0].patch, Some(Patch::Lazy { .. })),
            "non-deletion record must not be modified"
        );
    }
}
