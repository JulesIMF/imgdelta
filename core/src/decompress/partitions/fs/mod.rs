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

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use tempfile::TempDir;

use crate::decompress::context::DecompressContext;
use crate::decompress::PartitionDecompressStats;
use crate::manifest::{PartitionContent, PartitionManifest, Record};
use crate::partitions::PartitionHandle;
use crate::Result;

use context::DecompressContext as FsStageContext;
use draft::DecompressDraft;
use pipeline::DecompressPipeline;

pub use crate::decompress::partitions::PartitionDecompressor;

// ── FsPartitionDecompressor ───────────────────────────────────────────────────

/// Decompresses an Fs partition by running the 2-stage decompress pipeline.
///
/// Base mounting is handled here: if the output `FsHandle` has a base mount fn
/// injected via `set_base()` by the orchestrator, it is called via
/// `mount_base()` and the root is passed to the pipeline as `base_root`.
/// A temporary empty directory is used when there is no base partition
/// (full-image decompression).
///
/// `output_ph` must be `PartitionHandle::Fs(fh)` whose `mount_fn` provides an
/// **RW-mounted** (or directory-backed) filesystem root.  The mount is acquired
/// at the start of `decompress`, used for the pipeline, then dropped (which
/// triggers `umount2` for block-device backed mounts).
pub struct FsPartitionDecompressor;

#[async_trait]
impl PartitionDecompressor for FsPartitionDecompressor {
    async fn decompress(
        &self,
        ctx: &DecompressContext,
        pm: &PartitionManifest,
        output_ph: &PartitionHandle,
    ) -> Result<PartitionDecompressStats> {
        let fs_handle = match output_ph {
            PartitionHandle::Fs(h) => h,
            _ => unreachable!("FsPartitionDecompressor called with non-Fs handle"),
        };
        let records = match &pm.content {
            PartitionContent::Fs { records, .. } => records,
            _ => unreachable!("FsPartitionDecompressor called with non-Fs partition"),
        };

        // Mount base partition via the injected base_mount_fn (set by the
        // orchestrator before calling decompress), or use an empty TempDir
        // when there is no base image.
        let _base_mount: Option<Box<dyn crate::partitions::MountHandle>>;
        let _base_tmpdir: Option<TempDir>;
        let base_root: std::path::PathBuf = match fs_handle.mount_base() {
            Some(Ok(mount)) => {
                let path = mount.root().to_path_buf();
                _base_mount = Some(mount);
                _base_tmpdir = None;
                path
            }
            Some(Err(e)) => return Err(e),
            None => {
                let dir =
                    TempDir::new().map_err(|e| crate::Error::Other(format!("TempDir: {e}")))?;
                let path = dir.path().to_path_buf();
                _base_mount = None;
                _base_tmpdir = Some(dir);
                path
            }
        };

        // Acquire the output mount (RW for qcow2, simple dir handle for directory format).
        let output_mount = fs_handle.mount()?;
        let result = decompress_fs_partition(
            &base_root,
            output_mount.root(),
            records,
            Arc::clone(&ctx.patch_map),
            Arc::clone(&ctx.storage),
            Arc::clone(&ctx.router),
            ctx.workers,
        )
        .await;
        drop(output_mount); // triggers umount2 for block-device backed mounts
                            // _base_mount / _base_tmpdir drop here → umount / tmpdir cleanup.
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
    patch_map: Arc<HashMap<String, Vec<u8>>>,
    storage: Arc<dyn crate::storage::Storage>,
    router: Arc<crate::encoding::RouterEncoder>,
    workers: usize,
) -> Result<PartitionDecompressStats> {
    let ctx = FsStageContext {
        storage,
        router,
        workers,
        base_root: Arc::from(base_root),
        output_root: Arc::from(output_root),
        records: Arc::from(records),
        patch_map,
    };

    let pipeline = DecompressPipeline::default_fs();
    let draft = pipeline.run(&ctx, DecompressDraft::default()).await?;

    Ok(draft.stats)
}
