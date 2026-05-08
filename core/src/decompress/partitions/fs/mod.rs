// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// decompress/partitions/fs — FS partition decompressor and pipeline entry point

pub mod context;
pub mod draft;
pub mod pipeline;
pub mod stage;
pub mod stages;

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use crate::decompress::PartitionDecompressStats;
use crate::encoding::RouterEncoder;
use crate::manifest::{PartitionContent, PartitionManifest, Record};
use crate::partitions::PartitionHandle;
use crate::storage::Storage;
use crate::Result;

use context::DecompressContext;
use draft::DecompressDraft;
use pipeline::DecompressPipeline;

pub use crate::decompress::partitions::PartitionDecompressor;

// ── FsPartitionDecompressor ───────────────────────────────────────────────────

/// Decompresses an Fs partition by running the 3-stage decompress pipeline.
///
/// `output_ph` must be `PartitionHandle::Fs(fh)` whose `mount_fn` provides an
/// **RW-mounted** (or directory-backed) filesystem root.  The mount is acquired
/// at the start of `decompress`, used for the 3-stage pipeline, then dropped
/// (which triggers `umount2` for block-device backed mounts).
pub struct FsPartitionDecompressor;

#[async_trait]
impl PartitionDecompressor for FsPartitionDecompressor {
    async fn decompress(
        &self,
        pm: &PartitionManifest,
        base_root: &Path,
        output_ph: &PartitionHandle,
        storage: Arc<dyn Storage>,
        archive_bytes: &[u8],
        patches_compressed: bool,
        router: Arc<RouterEncoder>,
        workers: usize,
    ) -> Result<PartitionDecompressStats> {
        let fs_handle = match output_ph {
            PartitionHandle::Fs(h) => h,
            _ => unreachable!("FsPartitionDecompressor called with non-Fs handle"),
        };
        let records = match &pm.content {
            PartitionContent::Fs { records, .. } => records,
            _ => unreachable!("FsPartitionDecompressor called with non-Fs partition"),
        };
        // Acquire the output mount (RW for qcow2, simple dir handle for directory format).
        let output_mount = fs_handle.mount()?;
        let result = decompress_fs_partition(
            base_root,
            output_mount.root(),
            records,
            archive_bytes,
            patches_compressed,
            storage,
            router,
            workers,
        )
        .await;
        drop(output_mount); // triggers umount2 for block-device backed mounts
        result
    }
}

// ── decompress_fs_partition ───────────────────────────────────────────────────

/// Reconstruct an Fs partition into `output_root` from `base_root` + manifest records.
#[allow(clippy::too_many_arguments)]
pub async fn decompress_fs_partition(
    base_root: &Path,
    output_root: &Path,
    records: &[Record],
    archive_bytes: &[u8],
    patches_compressed: bool,
    storage: Arc<dyn Storage>,
    router: Arc<RouterEncoder>,
    workers: usize,
) -> Result<PartitionDecompressStats> {
    let ctx = DecompressContext {
        storage,
        router,
        workers,
        base_root: Arc::from(base_root),
        output_root: Arc::from(output_root),
        records: Arc::from(records),
        archive_bytes: Arc::from(archive_bytes),
        patches_compressed,
    };

    let pipeline = DecompressPipeline::default_fs();
    let draft = pipeline.run(&ctx, DecompressDraft::default()).await?;

    Ok(draft.stats)
}
