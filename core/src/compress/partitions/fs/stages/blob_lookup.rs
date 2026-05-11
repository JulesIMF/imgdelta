// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Compress pipeline: Stage 2 — blob_lookup

use std::collections::HashMap;

use async_trait::async_trait;
use tracing::debug;

use crate::compress::partitions::fs::context::StageContext;
use crate::compress::partitions::fs::draft::FsDraft;
use crate::compress::partitions::fs::stage::CompressStage;
use crate::manifest::{BlobRef, Data, DataRef, EntryType, Patch};
use crate::path_match::{find_best_matches, PathMatchConfig};
use crate::Result;

// ── Stage struct ──────────────────────────────────────────────────────────────

/// Stage 2: for each newly added regular file (`old_path = None, data = LazyBlob`),
/// find a matching blob in `base_image_id` via path similarity.
///
/// Files that find a match are upgraded to delta candidates:
/// `data = BlobRef(uuid)` and `patch = Lazy { old: BlobRef(uuid), new: FilePath(...) }`.
/// The patch will be computed in stage 7 with the blob as the delta base.
pub struct BlobLookup;

#[async_trait]
impl CompressStage for BlobLookup {
    fn name(&self) -> &'static str {
        "blob_lookup"
    }

    async fn run(&self, ctx: &StageContext, draft: FsDraft) -> Result<FsDraft> {
        let base_image_id = match &ctx.base_image_id {
            Some(id) => id.as_ref(),
            None => return Ok(draft),
        };
        blob_lookup_fn(
            draft,
            ctx.storage.as_ref(),
            base_image_id,
            ctx.partition_number,
        )
        .await
    }
}

// ── Implementation ────────────────────────────────────────────────────────────

pub async fn blob_lookup_fn(
    mut draft: FsDraft,
    storage: &dyn crate::storage::Storage,
    base_image_id: &str,
    partition_number: Option<i32>,
) -> Result<FsDraft> {
    let candidates = storage
        .find_blob_candidates(base_image_id, partition_number)
        .await?;
    if candidates.is_empty() {
        debug!("s3_lookup: no candidates in storage for base_image_id={base_image_id}");
        return Ok(draft);
    }
    debug!(
        base_image_id,
        candidates = candidates.len(),
        "s3_lookup: candidate blobs available"
    );

    let target_paths: Vec<String> = draft
        .records
        .iter()
        .filter(|r| {
            r.old_path.is_none()
                && r.entry_type == EntryType::File
                && matches!(r.data, Some(Data::LazyBlob(_)))
        })
        .filter_map(|r| r.new_path.clone())
        .collect();

    if target_paths.is_empty() {
        return Ok(draft);
    }

    let source_paths: Vec<String> = candidates.iter().map(|c| c.original_path.clone()).collect();
    let matches = find_best_matches(&source_paths, &target_paths, &PathMatchConfig::default())?;

    let match_map: HashMap<&str, uuid::Uuid> = matches
        .iter()
        .filter_map(|m| {
            candidates
                .iter()
                .find(|c| c.original_path == m.source_path)
                .map(|c| (m.target_path.as_str(), c.uuid))
        })
        .collect();

    for record in &mut draft.records {
        let new_path = match &record.new_path {
            Some(p) => p.clone(),
            None => continue,
        };
        if let Some(&blob_id) = match_map.get(new_path.as_str()) {
            if let Some(Data::LazyBlob(lazy_path)) = &record.data {
                let lazy_path = lazy_path.clone();
                let blob_ref = BlobRef {
                    blob_id,
                    size: record.size,
                };
                debug!(
                    path = %new_path,
                    blob_id = %blob_id,
                    "s3_lookup: matched to base blob"
                );
                record.patch = Some(Patch::Lazy {
                    old_data: DataRef::BlobRef(blob_ref.clone()),
                    new_data: DataRef::FilePath(lazy_path),
                });
                record.data = Some(Data::BlobRef(blob_ref));
            }
        }
    }

    Ok(draft)
}
