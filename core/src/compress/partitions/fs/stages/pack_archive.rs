// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Compress pipeline: Stage 8 — pack_and_upload_archive

use std::path::Path;

use async_trait::async_trait;
use flate2::write::GzEncoder;
use flate2::Compression;
use std::io::Write;

use crate::compress::partitions::fs::context::StageContext;
use crate::compress::partitions::fs::draft::FsDraft;
use crate::compress::partitions::fs::stage::CompressStage;
use crate::manifest::{PartitionContent, Patch, Record};
use crate::Result;

// ── Stage struct ──────────────────────────────────────────────────────────────

/// Stage 8: write per-patch files into `ctx.patches_dir` and build the
/// [`PartitionContent::Fs`] descriptor.
///
/// **Note:** This stage is NOT driven through [`CompressPipeline::run()`]
/// because it returns `PartitionContent` rather than [`FsDraft`].  The
/// entry-point [`compress_fs_partition`] calls it directly after the pipeline.
///
/// Patch files are written as `<patches_dir>/<key>`.  After all partitions are
/// processed the orchestrator calls [`pack_and_upload_patches`] once, which
/// reads every file from the shared patches directory and packs them into a
/// single archive.
///
/// [`CompressPipeline::run()`]: crate::compress::pipeline::CompressPipeline::run
/// [`compress_fs_partition`]: crate::compress::compress_fs_partition
pub struct PackAndUploadArchive;

#[async_trait]
impl CompressStage for PackAndUploadArchive {
    fn name(&self) -> &'static str {
        "pack_archive"
    }

    /// Not called from the pipeline runner — see struct docs.
    async fn run(&self, _ctx: &StageContext, draft: FsDraft) -> Result<FsDraft> {
        Ok(draft)
    }
}

impl PackAndUploadArchive {
    /// Execute Stage 8 (convenience wrapper used in tests).
    ///
    /// Creates a temporary directory, writes patch files there, packs and
    /// uploads the archive, then returns `(PartitionContent, compressed, stored_bytes)`.
    pub async fn pack_and_upload(
        draft: FsDraft,
        storage: &dyn crate::storage::Storage,
        image_id: &str,
        fs_type: &str,
    ) -> Result<(PartitionContent, bool, u64)> {
        pack_and_upload_archive_fn(draft, storage, image_id, fs_type, None).await
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Write each patch from `draft` as a separate file under `patches_dir`, then
/// build and return [`PartitionContent::Fs`].
///
/// Each patch is stored as `<patches_dir>/<key>` where `key` is the SHA-256
/// hex digest of the patch bytes.  The orchestrator calls
/// [`pack_and_upload_patches`] once after all partitions finish.
pub fn collect_fs_content(
    mut draft: FsDraft,
    fs_type: &str,
    fs_uuid: Option<String>,
    fs_mkfs_params: Option<std::collections::HashMap<String, String>>,
    patches_dir: &Path,
) -> Result<PartitionContent> {
    for (key, bytes) in draft.patch_bytes.drain() {
        let dest = patches_dir.join(&key);
        std::fs::write(&dest, &bytes)
            .map_err(|e| crate::Error::Other(format!("write patch {key}: {e}")))?;
    }

    let records: Vec<Record> = draft
        .records
        .into_iter()
        .filter(|r| !matches!(r.patch, Some(Patch::Lazy { .. })))
        .collect();

    Ok(PartitionContent::Fs {
        fs_type: fs_type.to_string(),
        fs_uuid,
        fs_mkfs_params,
        base_entity_count: draft.base_entity_count as u64,
        target_entity_count: draft.target_entity_count as u64,
        blobs_stored_bytes: draft.blobs_stored_bytes,
        records,
    })
}

/// Read every file from `patches_dir` (recursively), pack them into a tar
/// archive, optionally gzip it if that saves space, upload to storage, and
/// return `(archive_stored_bytes, patches_compressed)`.
///
/// Returns `(0, false)` immediately if `patches_dir` contains no files.
///
/// Called **once** by the orchestrator after all partitions are compressed.
pub async fn pack_and_upload_patches(
    patches_dir: &Path,
    storage: &dyn crate::storage::Storage,
    image_id: &str,
) -> Result<(u64, bool)> {
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    collect_files_recursive(patches_dir, patches_dir, &mut entries)?;

    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let tar_bytes = build_tar(entries)?;
    let (archive_bytes, compressed) = try_gzip(tar_bytes)?;
    let stored_bytes = archive_bytes.len() as u64;
    storage
        .upload_patches(image_id, &archive_bytes, compressed)
        .await?;
    Ok((stored_bytes, compressed))
}

/// Backward-compatible wrapper used by tests and
/// [`PackAndUploadArchive::pack_and_upload`].
///
/// Creates a temporary directory for patches, calls [`collect_fs_content`] +
/// [`pack_and_upload_patches`] in sequence.
pub async fn pack_and_upload_archive_fn(
    draft: FsDraft,
    storage: &dyn crate::storage::Storage,
    image_id: &str,
    fs_type: &str,
    fs_uuid: Option<String>,
) -> Result<(PartitionContent, bool, u64)> {
    let tmp = tempfile::TempDir::new()
        .map_err(|e| crate::Error::Other(format!("tempdir for patches: {e}")))?;
    let content = collect_fs_content(draft, fs_type, fs_uuid, None, tmp.path())?;
    let (stored_bytes, compressed) = pack_and_upload_patches(tmp.path(), storage, image_id).await?;
    Ok((content, compressed, stored_bytes))
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Recursively walk `dir`, appending `(relative_path_string, bytes)` to `out`.
fn collect_files_recursive(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(String, Vec<u8>)>,
) -> Result<()> {
    let rd = std::fs::read_dir(dir)
        .map_err(|e| crate::Error::Other(format!("read_dir {}: {e}", dir.display())))?;
    for entry in rd {
        let entry = entry.map_err(|e| crate::Error::Other(format!("read_dir entry: {e}")))?;
        let path = entry.path();
        let ft = entry
            .file_type()
            .map_err(|e| crate::Error::Other(format!("file_type: {e}")))?;
        if ft.is_dir() {
            collect_files_recursive(root, &path, out)?;
        } else if ft.is_file() {
            let rel = path
                .strip_prefix(root)
                .map_err(|e| crate::Error::Other(format!("strip_prefix: {e}")))?
                .to_string_lossy()
                .into_owned();
            let bytes = std::fs::read(&path)
                .map_err(|e| crate::Error::Other(format!("read {}: {e}", path.display())))?;
            out.push((rel, bytes));
        }
    }
    Ok(())
}

fn build_tar(entries: Vec<(String, Vec<u8>)>) -> Result<Vec<u8>> {
    let mut builder = tar::Builder::new(Vec::<u8>::new());
    for (name, bytes) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, &name, bytes.as_slice())
            .map_err(|e| crate::Error::Archive(format!("tar append: {e}")))?;
    }
    builder
        .into_inner()
        .map_err(|e| crate::Error::Archive(format!("tar finish: {e}")))
}

/// Attempt to gzip `bytes`.  Returns `(bytes, true)` if the compressed form is
/// smaller, `(original, false)` otherwise.
fn try_gzip(bytes: Vec<u8>) -> Result<(Vec<u8>, bool)> {
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    enc.write_all(&bytes)
        .map_err(|e| crate::Error::Archive(format!("gzip write: {e}")))?;
    let compressed = enc
        .finish()
        .map_err(|e| crate::Error::Archive(format!("gzip finish: {e}")))?;

    if compressed.len() < bytes.len() {
        Ok((compressed, true))
    } else {
        Ok((bytes, false))
    }
}
