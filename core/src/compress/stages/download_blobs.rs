// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Compress pipeline: Stage 6 — download_blobs_for_patches

use std::collections::HashMap;
use std::path::PathBuf;

use async_trait::async_trait;

use crate::compress::context::StageContext;
use crate::compress::stage::CompressStage;
use crate::compress::FsDraft;
use crate::manifest::{DataRef, Patch};
use crate::Result;

// ── Stage struct ──────────────────────────────────────────────────────────────

/// Stage 6: download blobs referenced as `DataRef::BlobRef` in `Patch::Lazy`
/// records to `tmp_dir`, replacing them with `DataRef::FilePath`.
///
/// After this stage every `Patch::Lazy` has both `old_data` and `new_data` as
/// `FilePath`s, so stage 7 can read them from disk without async I/O.
///
/// Downloaded files are recorded in [`FsDraft::tmp_files`] so the caller can
/// clean them up after patching.  Blobs referenced by multiple records are
/// downloaded only once (in-memory dedup within the call).
pub struct DownloadBlobsForPatches;

#[async_trait]
impl CompressStage for DownloadBlobsForPatches {
    fn name(&self) -> &'static str {
        "download_blobs"
    }

    async fn run(&self, ctx: &StageContext, draft: FsDraft) -> Result<FsDraft> {
        download_blobs_for_patches_fn(draft, ctx.storage.as_ref(), &ctx.tmp_dir).await
    }
}

// ── Implementation ────────────────────────────────────────────────────────────

pub async fn download_blobs_for_patches_fn(
    mut draft: FsDraft,
    storage: &dyn crate::storage::Storage,
    tmp_dir: &std::path::Path,
) -> Result<FsDraft> {
    let mut blob_cache: HashMap<uuid::Uuid, PathBuf> = HashMap::new();

    for record in &mut draft.records {
        let old_data = match &mut record.patch {
            Some(Patch::Lazy { old_data, .. }) => old_data,
            _ => continue,
        };
        let blob_id = match old_data {
            DataRef::BlobRef(br) => br.blob_id,
            DataRef::FilePath(_) => continue,
        };

        let tmp_path = if let Some(p) = blob_cache.get(&blob_id) {
            p.clone()
        } else {
            let bytes = storage.download_blob(blob_id).await?;
            let p = tmp_dir.join(blob_id.to_string());
            std::fs::write(&p, &bytes)?;
            draft.tmp_files.push(p.clone());
            blob_cache.insert(blob_id, p.clone());
            p
        };

        *old_data = DataRef::FilePath(tmp_path);
    }
    Ok(draft)
}
