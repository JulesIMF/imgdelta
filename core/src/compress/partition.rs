// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Compress pipeline: PartitionCompressor trait and partition-type implementations

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use sha2::{Digest, Sha256};

use crate::compress::context::StageContext;
use crate::image::PartitionHandle;
use crate::manifest::{BlobRef, PartitionContent, PartitionManifest};
use crate::partition::PartitionKind;
use crate::Result;

// ── Trait ─────────────────────────────────────────────────────────────────────

/// Handles compression for a single partition, regardless of type.
///
/// Implementations exist for:
/// - [`FsPartitionCompressor`] — mounts and runs the 8-stage pipeline.
/// - [`BiosBootCompressor`] — reads raw bytes and uploads as a single blob.
/// - [`RawPartitionCompressor`] — reads raw bytes and uploads as a single blob.
#[async_trait]
pub trait PartitionCompressor: Send + Sync {
    /// Compress one partition and return a ready [`PartitionManifest`].
    ///
    /// Also returns `(patches_compressed, archive_stored_bytes)` so the caller
    /// can accumulate totals.
    async fn compress(
        &self,
        ctx: &StageContext,
        handle: PartitionHandle,
        fs_type: &str,
        base_partitions: &HashMap<u32, PartitionHandle>,
        live_mounts: &mut Vec<Box<dyn crate::image::MountHandle>>,
        live_tmpdirs: &mut Vec<tempfile::TempDir>,
    ) -> Result<(PartitionManifest, bool, u64)>;
}

// ── FsPartitionCompressor ─────────────────────────────────────────────────────

/// Compresses a filesystem partition by running the 8-stage pipeline.
pub struct FsPartitionCompressor;

#[async_trait]
impl PartitionCompressor for FsPartitionCompressor {
    async fn compress(
        &self,
        ctx: &StageContext,
        handle: PartitionHandle,
        fs_type: &str,
        base_partitions: &HashMap<u32, PartitionHandle>,
        live_mounts: &mut Vec<Box<dyn crate::image::MountHandle>>,
        live_tmpdirs: &mut Vec<tempfile::TempDir>,
    ) -> Result<(PartitionManifest, bool, u64)> {
        let fs_handle = match handle {
            PartitionHandle::Fs(h) => h,
            _ => unreachable!("FsPartitionCompressor called with non-Fs handle"),
        };
        let descriptor = fs_handle.descriptor.clone();

        let target_mount = fs_handle.mount()?;
        let target_root_path: PathBuf = target_mount.root().to_path_buf();
        live_mounts.push(target_mount);

        let base_root_path: PathBuf = match base_partitions.get(&descriptor.number) {
            Some(PartitionHandle::Fs(base_fs)) => {
                let base_mount = base_fs.mount()?;
                let p = base_mount.root().to_path_buf();
                live_mounts.push(base_mount);
                p
            }
            _ => {
                let tmp = tempfile::TempDir::new()?;
                let p = tmp.path().to_path_buf();
                live_tmpdirs.push(tmp);
                p
            }
        };

        let partition_fs_type = match &descriptor.kind {
            PartitionKind::Fs { fs_type } => fs_type.clone(),
            _ => fs_type.to_string(),
        };

        super::compress_fs_partition(
            &base_root_path,
            &target_root_path,
            &descriptor,
            Arc::clone(&ctx.storage),
            &ctx.image_id,
            ctx.base_image_id.as_deref(),
            Arc::clone(&ctx.router),
            &partition_fs_type,
            ctx.workers,
        )
        .await
    }
}

// ── BiosBootCompressor ────────────────────────────────────────────────────────

/// Compresses a BIOS boot partition by uploading the raw bytes as a single blob.
pub struct BiosBootCompressor;

#[async_trait]
impl PartitionCompressor for BiosBootCompressor {
    async fn compress(
        &self,
        ctx: &StageContext,
        handle: PartitionHandle,
        _fs_type: &str,
        _base_partitions: &HashMap<u32, PartitionHandle>,
        _live_mounts: &mut Vec<Box<dyn crate::image::MountHandle>>,
        _live_tmpdirs: &mut Vec<tempfile::TempDir>,
    ) -> Result<(PartitionManifest, bool, u64)> {
        let bb_handle = match handle {
            PartitionHandle::BiosBoot(h) => h,
            _ => unreachable!("BiosBootCompressor called with non-BiosBoot handle"),
        };
        let descriptor = bb_handle.descriptor.clone();
        let bytes = bb_handle.read_raw()?;
        let sha256 = hex::encode(Sha256::digest(&bytes));
        let size = bytes.len() as u64;
        let blob_id = match ctx.storage.blob_exists(&sha256).await? {
            Some(id) => id,
            None => ctx.storage.upload_blob(&sha256, &bytes).await?,
        };
        Ok((
            PartitionManifest {
                descriptor,
                content: PartitionContent::BiosBoot {
                    blob_id,
                    sha256,
                    size,
                },
            },
            false,
            size,
        ))
    }
}

// ── RawPartitionCompressor ────────────────────────────────────────────────────

/// Compresses a raw partition by uploading the raw bytes as a single blob.
pub struct RawPartitionCompressor;

#[async_trait]
impl PartitionCompressor for RawPartitionCompressor {
    async fn compress(
        &self,
        ctx: &StageContext,
        handle: PartitionHandle,
        _fs_type: &str,
        _base_partitions: &HashMap<u32, PartitionHandle>,
        _live_mounts: &mut Vec<Box<dyn crate::image::MountHandle>>,
        _live_tmpdirs: &mut Vec<tempfile::TempDir>,
    ) -> Result<(PartitionManifest, bool, u64)> {
        let raw_handle = match handle {
            PartitionHandle::Raw(h) => h,
            _ => unreachable!("RawPartitionCompressor called with non-Raw handle"),
        };
        let descriptor = raw_handle.descriptor.clone();
        let bytes = raw_handle.read_raw()?;
        let sha256 = hex::encode(Sha256::digest(&bytes));
        let size = bytes.len() as u64;
        let blob_id = match ctx.storage.blob_exists(&sha256).await? {
            Some(id) => id,
            None => ctx.storage.upload_blob(&sha256, &bytes).await?,
        };
        Ok((
            PartitionManifest {
                descriptor,
                content: PartitionContent::Raw {
                    size,
                    blob: Some(BlobRef { blob_id, size }),
                    patch: None,
                },
            },
            false,
            size,
        ))
    }
}
