// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Compress pipeline: Stage 7 — compute_patches

use std::sync::Arc;

use async_trait::async_trait;
use rayon::prelude::*;
use tracing::debug;

use crate::compress::partitions::fs::context::StageContext;
use crate::compress::partitions::fs::draft::FsDraft;
use crate::compress::partitions::fs::stage::CompressStage;
use crate::encoding::router::FileInfo;
use crate::encoding::FileSnapshot;
use crate::encoding::PassthroughEncoder;
use crate::encoding::PatchEncoder;
use crate::manifest::{DataRef, EntryType, Patch, PatchRef};
use crate::Result;

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
    router: &crate::encoding::RouterEncoder,
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

                use crate::compress::partitions::fs::stages::upload_blobs::hex_sha256_bytes;
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

fn read_entry_bytes(data: &DataRef, entry_type: &EntryType) -> Result<Vec<u8>> {
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

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;
    use crate::encoding::RouterEncoder;
    use crate::encoding::{AlgorithmCode, Xdelta3Encoder};
    use crate::manifest::{EntryType, Patch, Record};

    fn write(dir: &std::path::Path, rel: &str, content: &[u8]) {
        let full = dir.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(full, content).unwrap();
    }

    fn make_xdelta3_router() -> RouterEncoder {
        RouterEncoder::new(vec![], Arc::new(Xdelta3Encoder::new()))
    }

    #[test]
    fn test_compute_patches_xdelta3() {
        let base_dir = tempfile::TempDir::new().unwrap();
        let target_dir = tempfile::TempDir::new().unwrap();
        write(base_dir.path(), "lib/libz.so.1", b"base content of libz");
        write(
            target_dir.path(),
            "lib/libz.so.1",
            b"updated content of libz v2",
        );

        let mut draft = FsDraft::default();
        draft.records.push(Record {
            old_path: Some("lib/libz.so.1".into()),
            new_path: Some("lib/libz.so.1".into()),
            entry_type: EntryType::File,
            size: 26,
            data: None,
            patch: Some(Patch::Lazy {
                old_data: DataRef::FilePath(base_dir.path().join("lib/libz.so.1")),
                new_data: DataRef::FilePath(target_dir.path().join("lib/libz.so.1")),
            }),
            metadata: None,
        });

        let router = make_xdelta3_router();
        let draft = compute_patches_fn(draft, &router, 4).unwrap();

        let record = &draft.records[0];
        assert!(
            matches!(record.patch, Some(Patch::Real(_))),
            "Lazy patch should become Real after compute_patches"
        );
        let pref = match &record.patch {
            Some(Patch::Real(p)) => p,
            _ => unreachable!(),
        };
        assert_eq!(pref.algorithm_code, AlgorithmCode::Xdelta3);
        assert!(!pref.sha256.is_empty());
        assert!(
            draft.patch_bytes.contains_key(&pref.archive_entry),
            "patch bytes must be stored"
        );
    }

    #[test]
    fn test_compute_patches_passthrough_symlink() {
        let base_dir = tempfile::TempDir::new().unwrap();
        let target_dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(base_dir.path().join("usr/bin")).unwrap();
        std::fs::create_dir_all(target_dir.path().join("usr/bin")).unwrap();
        symlink(
            "/usr/bin/python3.10",
            base_dir.path().join("usr/bin/python"),
        )
        .unwrap();
        symlink(
            "/usr/bin/python3.11",
            target_dir.path().join("usr/bin/python"),
        )
        .unwrap();

        let mut draft = FsDraft::default();
        draft.records.push(Record {
            old_path: Some("usr/bin/python".into()),
            new_path: Some("usr/bin/python".into()),
            entry_type: EntryType::Symlink,
            size: 0,
            data: None,
            patch: Some(Patch::Lazy {
                old_data: DataRef::FilePath(base_dir.path().join("usr/bin/python")),
                new_data: DataRef::FilePath(target_dir.path().join("usr/bin/python")),
            }),
            metadata: None,
        });

        let router = make_xdelta3_router();
        let draft = compute_patches_fn(draft, &router, 4).unwrap();

        let pref = match &draft.records[0].patch {
            Some(Patch::Real(p)) => p,
            _ => panic!("expected Patch::Real"),
        };
        assert_eq!(
            pref.algorithm_code,
            AlgorithmCode::Passthrough,
            "symlink patch must use passthrough encoder"
        );

        // Verify: decoded patch = new link target bytes.
        let patch_bytes = &draft.patch_bytes[&pref.archive_entry];
        let decoded = String::from_utf8(patch_bytes.clone()).unwrap();
        assert_eq!(decoded, "/usr/bin/python3.11");
    }

    #[test]
    fn test_compute_patches_no_lazy_patches_is_noop() {
        let mut draft = FsDraft::default();
        draft.records.push(Record {
            old_path: Some("etc/removed".into()),
            new_path: None,
            entry_type: EntryType::File,
            size: 0,
            data: None,
            patch: None,
            metadata: None,
        });

        let router = make_xdelta3_router();
        let result = compute_patches_fn(draft, &router, 1).unwrap();

        assert!(result.patch_bytes.is_empty());
    }
}
