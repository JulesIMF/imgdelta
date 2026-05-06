// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Compress pipeline: module root and public entry point

//! Eight-stage stateless compress pipeline for one `Fs` partition.
//!
//! The public entry point [`compress_fs_partition`] has the same signature as
//! the old `compress_pipeline::compress_fs_partition` and is a drop-in replacement.

pub mod context;
pub mod draft;
pub mod partition;
pub mod pipeline;
pub mod stage;
pub mod stages;

pub use draft::FsDraft;

// Stage functions re-exported under their short names for direct use.
pub use stages::cleanup::cleanup_fn as cleanup;
pub use stages::compute_patches::compute_patches_fn as compute_patches;
pub use stages::download_blobs::download_blobs_for_patches_fn as download_blobs_for_patches;
pub use stages::match_renamed::match_renamed_fn as match_renamed;
pub use stages::pack_archive::pack_and_upload_archive_fn as pack_and_upload_archive;
pub use stages::s3_lookup::s3_lookup_fn as s3_lookup;
pub use stages::upload_blobs::upload_lazy_blobs_fn as upload_lazy_blobs;
pub use stages::walkdir::walkdir_fn as walkdir;

use std::path::Path;
use std::sync::Arc;

use crate::manifest::PartitionManifest;
use crate::partition::PartitionDescriptor;
use crate::routing::RouterEncoder;
use crate::storage::Storage;
use crate::Result;

use context::StageContext;
use pipeline::CompressPipeline;
use stages::pack_archive::pack_and_upload_archive_fn;
use stages::walkdir::walkdir_fn;

// ── Public entry point ────────────────────────────────────────────────────────

/// Run the full 8-stage compress pipeline for one Fs partition.
///
/// Drop-in replacement for `compress_pipeline::compress_fs_partition`.
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
    workers: usize,
) -> Result<(PartitionManifest, bool, u64)> {
    let tmp_dir = tempfile::TempDir::new()?;

    // Stage 1: walkdir (needs filesystem paths, called outside the pipeline).
    let draft = walkdir_fn(base_root, target_root)?;

    // Stages 2–7 via the pipeline runner.
    let ctx = StageContext {
        storage: Arc::clone(&storage),
        router,
        image_id: Arc::from(image_id),
        base_image_id: base_image_id.map(Arc::from),
        partition_number: Some(descriptor.number as i32),
        workers,
        tmp_dir: Arc::from(tmp_dir.path()),
    };

    let pipeline = CompressPipeline::default_fs();
    let draft = pipeline.run(&ctx, draft, None).await?;

    // Stage 8: pack and upload archive.
    let (content, patches_compressed, archive_stored_bytes) =
        pack_and_upload_archive_fn(draft, storage.as_ref(), image_id, fs_type).await?;

    Ok((
        PartitionManifest {
            descriptor: descriptor.clone(),
            content,
        },
        patches_compressed,
        archive_stored_bytes,
    ))
}
