// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// compress/partitions/fs — FS partition compressor and 8-stage pipeline

pub mod context;
pub mod draft;
pub mod pipeline;
pub mod stage;
pub mod stages;

pub use draft::FsDraft;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use tracing::info;

use crate::compress::context::CompressContext;
use crate::encoding::RouterEncoder;
use crate::manifest::PartitionManifest;
use crate::partitions::{PartitionDescriptor, PartitionHandle, PartitionKind};
use crate::storage::Storage;
use crate::Result;

use context::StageContext;
use pipeline::CompressPipeline;
use stages::pack_archive::{collect_fs_content, pack_and_upload_patches};
use stages::walkdir::walkdir_fn;

// ── FsPartitionCompressor ─────────────────────────────────────────────────────

use crate::compress::partitions::PartitionCompressor;

/// Compresses a filesystem partition by running the 8-stage pipeline.
///
/// Patch files are written to `ctx.patches_dir`.  The orchestrator packs them
/// into a single archive after all partitions finish.
pub struct FsPartitionCompressor;

#[async_trait]
impl PartitionCompressor for FsPartitionCompressor {
    async fn compress(
        &self,
        ctx: &CompressContext,
        handle: PartitionHandle,
    ) -> Result<PartitionManifest> {
        let fs_handle = match handle {
            PartitionHandle::Fs(h) => h,
            _ => unreachable!("FsPartitionCompressor called with non-Fs handle"),
        };
        let descriptor = fs_handle.descriptor.clone();

        info!(
            partition = descriptor.number,
            "FsPartitionCompressor: mounting target partition"
        );
        let t_mount = Instant::now();
        let target_mount = fs_handle.mount()?;
        info!(
            partition = descriptor.number,
            elapsed_ms = t_mount.elapsed().as_millis(),
            "FsPartitionCompressor: target mounted"
        );
        let target_root_path: PathBuf = target_mount.root().to_path_buf();
        // Keep mount alive until compress_fs_partition returns.
        let _target_mount = target_mount;

        // Base root: use the injected base mount fn if present, otherwise an
        // empty tmp directory (full-image compress).
        let _base_mount_guard: Option<Box<dyn crate::partitions::MountHandle>>;
        let _base_tmpdir: Option<tempfile::TempDir>;
        let base_root_path: PathBuf = match fs_handle.mount_base() {
            Some(mount_result) => {
                info!(
                    partition = descriptor.number,
                    "FsPartitionCompressor: mounting base partition"
                );
                let base_mount = mount_result?;
                info!(
                    partition = descriptor.number,
                    "FsPartitionCompressor: base mounted"
                );
                let p = base_mount.root().to_path_buf();
                _base_mount_guard = Some(base_mount);
                _base_tmpdir = None;
                p
            }
            None => {
                let tmp = tempfile::TempDir::new()?;
                let p = tmp.path().to_path_buf();
                _base_mount_guard = None;
                _base_tmpdir = Some(tmp);
                p
            }
        };

        let partition_fs_type = match &descriptor.kind {
            PartitionKind::Fs { fs_type } => fs_type.clone(),
            _ => "unknown".into(),
        };

        let fs_uuid = fs_handle.fs_uuid.clone();
        let fs_mkfs_params = fs_handle.fs_mkfs_params.clone();

        compress_fs_partition(
            &base_root_path,
            &target_root_path,
            &descriptor,
            Arc::clone(&ctx.storage),
            &ctx.image_id,
            ctx.base_image_id.as_deref(),
            Arc::clone(&ctx.router),
            &partition_fs_type,
            fs_uuid,
            fs_mkfs_params,
            ctx.workers,
            ctx.debug_dir.as_deref(),
            &ctx.patches_dir,
        )
        .await
    }
}

// ── compress_fs_partition ─────────────────────────────────────────────────────

/// Run the full 8-stage compress pipeline for one Fs partition.
///
/// Patch files are written as `<patches_dir>/<key>`.  The orchestrator
/// accumulates all partition patches in a shared directory and calls
/// [`pack_and_upload_patches`] once after the loop.
#[allow(clippy::too_many_arguments)]
pub async fn compress_fs_partition(
    base_root: &Path,
    target_root: &Path,
    descriptor: &PartitionDescriptor,
    storage: Arc<dyn Storage>,
    image_id: &str,
    base_image_id: Option<&str>,
    router: Arc<RouterEncoder>,
    fs_type: &str,
    fs_uuid: Option<String>,
    fs_mkfs_params: Option<std::collections::HashMap<String, String>>,
    workers: usize,
    debug_dir: Option<&Path>,
    patches_dir: &Path,
) -> Result<PartitionManifest> {
    let tmp_dir = tempfile::TempDir::new()?;

    let base = base_root.to_path_buf();
    let target = target_root.to_path_buf();
    let t0 = Instant::now();
    info!(base = %base.display(), target = %target.display(), "walkdir: starting (may take several minutes for large partitions)");
    let draft = tokio::task::spawn_blocking(move || walkdir_fn(&base, &target))
        .await
        .map_err(|e| crate::Error::Other(format!("walkdir task panicked: {e}")))??;
    info!(
        records = draft.records.len(),
        elapsed_s = format!("{:.1}", t0.elapsed().as_secs_f64()),
        "walkdir: done"
    );

    let ctx = StageContext {
        storage: Arc::clone(&storage),
        router,
        image_id: Arc::from(image_id),
        base_image_id: base_image_id.map(Arc::from),
        partition_number: Some(descriptor.number as i32),
        workers,
        tmp_dir: Arc::from(tmp_dir.path()),
        patches_dir: Arc::from(patches_dir),
        debug_dir: debug_dir.map(Arc::from),
    };

    let pipeline = CompressPipeline::default_fs();
    let draft = pipeline.run(&ctx, draft, debug_dir).await?;

    let content = collect_fs_content(draft, fs_type, fs_uuid, fs_mkfs_params, patches_dir)?;

    Ok(PartitionManifest {
        descriptor: descriptor.clone(),
        content,
    })
}

// ── Backward-compatible wrapper used by tests ─────────────────────────────────

/// Like [`compress_fs_partition`] but also packs and uploads the patches
/// archive immediately, returning `(PartitionManifest, patches_compressed,
/// archive_stored_bytes)`.
///
/// Creates a temporary directory for patches, so each call is self-contained.
/// Use this in tests written against the old single-partition API.
#[allow(clippy::too_many_arguments)]
pub async fn compress_fs_partition_and_upload(
    base_root: &Path,
    target_root: &Path,
    descriptor: &PartitionDescriptor,
    storage: Arc<dyn Storage>,
    image_id: &str,
    base_image_id: Option<&str>,
    router: Arc<RouterEncoder>,
    fs_type: &str,
    fs_uuid: Option<String>,
    workers: usize,
    debug_dir: Option<&Path>,
) -> Result<(PartitionManifest, bool, u64)> {
    let patches_tmp = tempfile::TempDir::new()
        .map_err(|e| crate::Error::Other(format!("tempdir for patches: {e}")))?;
    let pm = compress_fs_partition(
        base_root,
        target_root,
        descriptor,
        Arc::clone(&storage),
        image_id,
        base_image_id,
        Arc::clone(&router),
        fs_type,
        fs_uuid,
        None, // fs_mkfs_params not available in this test-facing wrapper
        workers,
        debug_dir,
        patches_tmp.path(),
    )
    .await?;
    let (stored_bytes, compressed) =
        pack_and_upload_patches(patches_tmp.path(), storage.as_ref(), image_id).await?;
    Ok((pm, compressed, stored_bytes))
}
