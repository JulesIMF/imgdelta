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

use std::path::Path;

use crate::manifest::PartitionManifest;
use crate::partition::PartitionDescriptor;
use crate::routing::RouterEncoder;
use crate::storage::Storage;
use crate::Result;
use tracing::info;

use stages::cleanup::cleanup_fn;
use stages::compute_patches::compute_patches_fn;
use stages::download_blobs::download_blobs_for_patches_fn;
use stages::match_renamed::match_renamed_fn;
use stages::pack_archive::pack_and_upload_archive_fn;
use stages::s3_lookup::s3_lookup_fn;
use stages::upload_blobs::upload_lazy_blobs_fn;
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
    storage: &dyn Storage,
    image_id: &str,
    base_image_id: Option<&str>,
    router: &RouterEncoder,
    fs_type: &str,
    workers: usize,
) -> Result<(PartitionManifest, bool, u64)> {
    let tmp_dir = tempfile::TempDir::new()?;

    info!(
        image_id,
        base_image_id,
        partition = descriptor.number,
        "stage 1/8: walkdir"
    );
    let draft = walkdir_fn(base_root, target_root)?;
    let n_records = draft.records.len();
    info!(
        image_id,
        partition = descriptor.number,
        records = n_records,
        "stage 1/8: walkdir done"
    );

    let draft = if let Some(base_id) = base_image_id {
        info!(
            image_id,
            base_image_id = base_id,
            partition = descriptor.number,
            "stage 2/8: s3_lookup"
        );
        let d = s3_lookup_fn(draft, storage, base_id, Some(descriptor.number as i32)).await?;
        info!(
            image_id,
            partition = descriptor.number,
            "stage 2/8: s3_lookup done"
        );
        d
    } else {
        draft
    };

    info!(
        image_id,
        partition = descriptor.number,
        "stage 3/8: match_renamed"
    );
    let draft = match_renamed_fn(draft, 0.85);
    let n_renamed = draft
        .records
        .iter()
        .filter(|r| r.old_path.is_some() && r.new_path.is_some() && r.old_path != r.new_path)
        .count();
    info!(
        image_id,
        partition = descriptor.number,
        renamed = n_renamed,
        "stage 3/8: match_renamed done"
    );

    info!(
        image_id,
        partition = descriptor.number,
        "stage 4/8: cleanup"
    );
    let draft = cleanup_fn(draft);

    info!(
        image_id,
        partition = descriptor.number,
        "stage 5/8: upload_blobs"
    );
    let draft = upload_lazy_blobs_fn(
        draft,
        storage,
        image_id,
        base_image_id,
        Some(descriptor.number as i32),
    )
    .await?;

    info!(
        image_id,
        partition = descriptor.number,
        "stage 6/8: download_blobs"
    );
    let draft = download_blobs_for_patches_fn(draft, storage, tmp_dir.path()).await?;

    let n_patches = draft
        .records
        .iter()
        .filter(|r| matches!(r.patch, Some(crate::manifest::Patch::Lazy { .. })))
        .count();
    info!(
        image_id,
        partition = descriptor.number,
        patches = n_patches,
        "stage 7/8: compute_patches"
    );
    let draft = compute_patches_fn(draft, router, workers)?;

    for p in &draft.tmp_files {
        let _ = std::fs::remove_file(p);
    }
    let mut draft = draft;
    draft.tmp_files.clear();

    info!(
        image_id,
        partition = descriptor.number,
        "stage 8/8: pack_archive"
    );
    let (content, patches_compressed, archive_stored_bytes) =
        pack_and_upload_archive_fn(draft, storage, image_id, fs_type).await?;

    info!(
        image_id,
        partition = descriptor.number,
        patches_compressed,
        archive_stored_bytes,
        "pipeline complete"
    );

    Ok((
        PartitionManifest {
            descriptor: descriptor.clone(),
            content,
        },
        patches_compressed,
        archive_stored_bytes,
    ))
}
