// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Compress pipeline: Stage 8 — pack_and_upload_archive

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

/// Stage 8: pack all patch bytes into a tar archive, optionally gzip-compress
/// it if that saves space, upload it to storage, and return
/// `(PartitionContent::Fs, patches_compressed, archive_stored_bytes)`.
///
/// **Note:** This stage is NOT driven through [`CompressPipeline::run()`] because
/// it returns `(PartitionContent, bool, u64)` rather than [`FsDraft`].  The
/// entry-point [`compress_fs_partition`] calls it directly after the pipeline.
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
    /// Execute Stage 8.
    ///
    /// Consumes `draft`, builds and uploads the patches archive, and returns
    /// `(PartitionContent, patches_compressed, archive_stored_bytes)`.
    pub async fn pack_and_upload(
        draft: FsDraft,
        storage: &dyn crate::storage::Storage,
        image_id: &str,
        fs_type: &str,
    ) -> Result<(PartitionContent, bool, u64)> {
        pack_and_upload_archive_fn(draft, storage, image_id, fs_type, None).await
    }
}

// ── Implementation ────────────────────────────────────────────────────────────

pub async fn pack_and_upload_archive_fn(
    mut draft: FsDraft,
    storage: &dyn crate::storage::Storage,
    image_id: &str,
    fs_type: &str,
    fs_uuid: Option<String>,
) -> Result<(PartitionContent, bool, u64)> {
    let tar_bytes = {
        let mut builder = tar::Builder::new(Vec::<u8>::new());
        let mut entries: Vec<(String, Vec<u8>)> = draft.patch_bytes.drain().collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
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
            .map_err(|e| crate::Error::Archive(format!("tar finish: {e}")))?
    };

    let (archive_bytes, compressed) = try_gzip(tar_bytes)?;
    let archive_stored_bytes = archive_bytes.len() as u64;

    storage
        .upload_patches(image_id, &archive_bytes, compressed)
        .await?;

    let records: Vec<Record> = draft
        .records
        .into_iter()
        .filter(|r| !matches!(r.patch, Some(Patch::Lazy { .. })))
        .collect();

    Ok((
        PartitionContent::Fs {
            fs_type: fs_type.to_string(),
            fs_uuid,
            records,
        },
        compressed,
        archive_stored_bytes,
    ))
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
