// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// operations::delete — free-function orchestrator for image deletion

use std::collections::HashSet;
use std::sync::Arc;

use tracing::info;

use super::{DeleteOptions, DeleteStats};
use crate::manifest::{Data, Manifest, PartitionContent};
use crate::{Result, Storage};

/// Delete a stored image and all data exclusively owned by it.
///
/// Shared blobs (referenced by other images) are left intact.
/// Safe ordering: blob_origins → patches → manifest → image_meta.
pub async fn delete_image(
    storage: Arc<dyn Storage>,
    options: DeleteOptions,
) -> Result<DeleteStats> {
    let image_id = &options.image_id;

    // 1. Verify image exists.
    storage
        .get_image(image_id)
        .await?
        .ok_or_else(|| crate::Error::Other(format!("image not found: {image_id}")))?;

    // 2. Refuse to delete if any other image uses this one as a base.
    let all_images = storage.list_images().await?;
    let children: Vec<_> = all_images
        .iter()
        .filter(|m| m.base_image_id.as_deref() == Some(image_id))
        .collect();
    if !children.is_empty() {
        let names: Vec<_> = children.iter().map(|m| m.image_id.as_str()).collect();
        return Err(crate::Error::Other(format!(
            "cannot delete {image_id}: it is the base for [{}]",
            names.join(", ")
        )));
    }

    // 3. Collect blob IDs referenced by THIS image.
    let manifest_bytes = storage.download_manifest(image_id).await?;
    let this_manifest = Manifest::from_bytes(&manifest_bytes)
        .map_err(|e| crate::Error::Other(format!("manifest decode: {e}")))?;
    let this_blobs = collect_manifest_blob_ids(&this_manifest);

    // 4. Collect blob IDs referenced by ALL OTHER images.
    let mut in_use_blobs = HashSet::<uuid::Uuid>::new();
    for other in &all_images {
        if other.image_id == *image_id {
            continue;
        }
        match storage.download_manifest(&other.image_id).await {
            Ok(bytes) => {
                if let Ok(m) = Manifest::from_bytes(&bytes) {
                    in_use_blobs.extend(collect_manifest_blob_ids(&m));
                }
            }
            Err(e) => {
                tracing::warn!(
                    image_id = other.image_id.as_str(),
                    error = %e,
                    "delete_image: could not read manifest for sibling image, skipping"
                );
            }
        }
    }

    // 5. Delete blobs that are exclusively owned by this image.
    let mut stats = DeleteStats::default();
    for blob_id in &this_blobs {
        if in_use_blobs.contains(blob_id) {
            tracing::debug!(%blob_id, "delete_image: blob still in use, keeping");
            stats.blobs_kept += 1;
        } else if options.dry_run {
            tracing::info!(%blob_id, "delete_image: dry-run, would delete blob");
            stats.blobs_deleted += 1;
        } else {
            storage.delete_blob(*blob_id).await?;
            tracing::debug!(%blob_id, "delete_image: blob deleted");
            stats.blobs_deleted += 1;
        }
    }

    if !options.dry_run {
        // 6. Remove blob_origins rows for this image.
        storage.delete_blob_origins(image_id).await?;
        // 7. Remove patches archive.
        storage.delete_patches(image_id).await?;
        // 8. Remove manifest.
        storage.delete_manifest(image_id).await?;
        // 9. Remove image metadata record.
        storage.delete_image_meta(image_id).await?;
    }

    info!(
        image_id,
        blobs_deleted = stats.blobs_deleted,
        blobs_kept = stats.blobs_kept,
        dry_run = options.dry_run,
        "delete_image: done"
    );
    Ok(stats)
}

fn collect_manifest_blob_ids(manifest: &Manifest) -> HashSet<uuid::Uuid> {
    let mut ids = HashSet::new();
    for pm in &manifest.partitions {
        match &pm.content {
            PartitionContent::BiosBoot { blob_id, .. }
            | PartitionContent::MbrBootCode { blob_id, .. } => {
                ids.insert(*blob_id);
            }
            PartitionContent::Raw { blob, .. } => {
                if let Some(b) = blob {
                    ids.insert(b.blob_id);
                }
            }
            PartitionContent::Fs { records, .. } => {
                for r in records {
                    if let Some(Data::BlobRef(b)) = &r.data {
                        ids.insert(b.blob_id);
                    }
                }
            }
        }
    }
    ids
}
