// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Compress pipeline: Stage 7 — compute_patches

use std::sync::Arc;

use async_trait::async_trait;
use rayon::prelude::*;
use tracing::debug;

use crate::algorithm::FileSnapshot;
use crate::compress::context::StageContext;
use crate::compress::stage::CompressStage;
use crate::compress::FsDraft;
use crate::encoder::PatchEncoder;
use crate::manifest::{DataRef, EntryType, Patch, PatchRef};
use crate::routing::FileInfo;
use crate::{PassthroughEncoder, Result};

// ── Stage struct ──────────────────────────────────────────────────────────────

/// Stage 7: compute all binary patches in parallel (rayon) and populate
/// [`FsDraft::patch_bytes`].
///
/// For each record with `Patch::Lazy { old_data: FilePath, new_data: FilePath }`:
/// - Reads source and target bytes from disk.
/// - Encodes via the router (symlinks and hardlinks always use [`PassthroughEncoder`]).
/// - Stores raw patch bytes in `draft.patch_bytes` under the archive-entry name.
/// - Replaces `Patch::Lazy` with `Patch::Real`.
pub struct ComputePatches;

#[async_trait]
impl CompressStage for ComputePatches {
    fn name(&self) -> &'static str {
        "compute_patches"
    }

    async fn run(&self, ctx: &StageContext, draft: FsDraft) -> Result<FsDraft> {
        compute_patches_fn(draft, &ctx.router, ctx.workers)
    }
}

// ── Implementation ────────────────────────────────────────────────────────────

pub fn compute_patches_fn(
    mut draft: FsDraft,
    router: &crate::routing::RouterEncoder,
    workers: usize,
) -> Result<FsDraft> {
    let needs_patch: Vec<usize> = draft
        .records
        .iter()
        .enumerate()
        .filter(|(_, r)| matches!(r.patch, Some(Patch::Lazy { .. })))
        .map(|(i, _)| i)
        .collect();

    if needs_patch.is_empty() {
        return Ok(draft);
    }

    let passthrough: Arc<dyn PatchEncoder> = Arc::new(PassthroughEncoder::new());

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(workers)
        .build()
        .map_err(|e| crate::Error::Other(format!("failed to build rayon pool: {e}")))?;

    type PatchResult = Result<(usize, PatchRef, Vec<u8>)>;
    let results: Vec<PatchResult> = pool.install(|| {
        needs_patch
            .par_iter()
            .map(|&i| {
                let record = &draft.records[i];
                let (old_data, new_data) = match &record.patch {
                    Some(Patch::Lazy { old_data, new_data }) => (old_data, new_data),
                    _ => unreachable!(),
                };

                let old_bytes = read_entry_bytes(old_data, &record.entry_type)?;
                let new_bytes = read_entry_bytes(new_data, &record.entry_type)?;

                let new_path_str = record.new_path.as_deref().unwrap_or("");
                let header_slice: &[u8] = &new_bytes[..new_bytes.len().min(16)];

                let encoder: Arc<dyn PatchEncoder> =
                    if matches!(record.entry_type, EntryType::Symlink | EntryType::Hardlink) {
                        Arc::clone(&passthrough)
                    } else {
                        router.select(&FileInfo {
                            path: new_path_str,
                            size: new_bytes.len() as u64,
                            header: header_slice,
                        })
                    };

                let base_snap = FileSnapshot {
                    path: record.old_path.as_deref().unwrap_or(""),
                    size: old_bytes.len() as u64,
                    header: &old_bytes[..old_bytes.len().min(16)],
                    bytes: &old_bytes,
                };
                let target_snap = FileSnapshot {
                    path: new_path_str,
                    size: new_bytes.len() as u64,
                    header: header_slice,
                    bytes: &new_bytes,
                };

                let file_patch = encoder.encode(&base_snap, &target_snap)?;

                use crate::compress::stages::upload_blobs::hex_sha256_bytes;
                let sha256 = hex_sha256_bytes(&file_patch.bytes);
                let archive_entry = format!("{:06}.patch", i);
                let pref = PatchRef {
                    archive_entry: archive_entry.clone(),
                    sha256,
                    algorithm_code: file_patch.code,
                    algorithm_id: file_patch.algorithm_id.clone(),
                };

                debug!(
                    path = new_path_str,
                    algorithm = file_patch.algorithm_id.as_deref().unwrap_or("unknown"),
                    patch_bytes = file_patch.bytes.len(),
                    "patch computed"
                );

                Ok((i, pref, file_patch.bytes))
            })
            .collect()
    });

    for res in results {
        let (idx, pref, bytes) = res?;
        let key = pref.archive_entry.clone();
        draft.records[idx].patch = Some(Patch::Real(pref));
        draft.patch_bytes.insert(key, bytes);
    }

    Ok(draft)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

pub(crate) fn read_entry_bytes(data: &DataRef, entry_type: &EntryType) -> Result<Vec<u8>> {
    match data {
        DataRef::FilePath(path) => {
            if *entry_type == EntryType::Symlink {
                Ok(std::fs::read_link(path)?
                    .to_string_lossy()
                    .into_owned()
                    .into_bytes())
            } else {
                Ok(std::fs::read(path)?)
            }
        }
        DataRef::BlobRef(_) => Err(crate::Error::Encode(
            "BlobRef in compute_patches: call download_blobs_for_patches first".into(),
        )),
    }
}
