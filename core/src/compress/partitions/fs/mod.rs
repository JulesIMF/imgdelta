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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use tracing::info;

use crate::manifest::PartitionManifest;
use crate::partition::{PartitionDescriptor, PartitionKind};
use crate::partitions::{MountHandle, PartitionHandle};
use crate::routing::RouterEncoder;
use crate::storage::Storage;
use crate::Result;

use context::StageContext;
use pipeline::CompressPipeline;
use stages::pack_archive::pack_and_upload_archive_fn;
use stages::walkdir::walkdir_fn;

// ── FsPartitionCompressor ─────────────────────────────────────────────────────

use crate::compress::partitions::PartitionCompressor;

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
        live_mounts: &mut Vec<Box<dyn MountHandle>>,
        live_tmpdirs: &mut Vec<tempfile::TempDir>,
    ) -> Result<(PartitionManifest, bool, u64)> {
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
        live_mounts.push(target_mount);

        let base_root_path: PathBuf = match base_partitions.get(&descriptor.number) {
            Some(PartitionHandle::Fs(base_fs)) => {
                info!(
                    partition = descriptor.number,
                    "FsPartitionCompressor: mounting base partition"
                );
                let base_mount = base_fs.mount()?;
                info!(
                    partition = descriptor.number,
                    "FsPartitionCompressor: base mounted"
                );
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

        let fs_uuid = fs_handle.fs_uuid.clone();

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
            ctx.workers,
            ctx.debug_dir.as_deref(),
        )
        .await
    }
}

// ── compress_fs_partition ─────────────────────────────────────────────────────

/// Run the full 8-stage compress pipeline for one Fs partition.
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
    workers: usize,
    debug_dir: Option<&Path>,
) -> Result<(PartitionManifest, bool, u64)> {
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
        debug_dir: debug_dir.map(Arc::from),
    };

    let pipeline = CompressPipeline::default_fs();
    let draft = pipeline.run(&ctx, draft, debug_dir).await?;

    let (content, patches_compressed, archive_stored_bytes) =
        pack_and_upload_archive_fn(draft, storage.as_ref(), image_id, fs_type, fs_uuid).await?;

    Ok((
        PartitionManifest {
            descriptor: descriptor.clone(),
            content,
        },
        patches_compressed,
        archive_stored_bytes,
    ))
}
