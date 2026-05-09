// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// operations::decompress — free-function orchestrator for the decompress pipeline

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use tracing::{error, info};

use super::{DecompressOptions, DecompressionStats};
use crate::decompress::partitions::fs::stages::extract_archive_fn;
use crate::decompress::{
    BiosBootDecompressor, DecompressContext, FsPartitionDecompressor, MbrDecompressor,
    PartitionDecompressor, RawPartitionDecompressor,
};
use crate::manifest::{PartitionContent, MANIFEST_VERSION};
use crate::partitions::{FsHandle, PartitionHandle};
use crate::storage::ImageStatus;
use crate::{Image, Result, Storage};

/// Reconstruct a target image from its stored delta and a base image.
pub async fn decompress(
    image_format: Arc<dyn Image>,
    storage: Arc<dyn Storage>,
    router: Arc<crate::encoding::RouterEncoder>,
    output_root: &Path,
    options: DecompressOptions,
) -> Result<DecompressionStats> {
    let started_at = Instant::now();
    let image_id = &options.image_id;

    info!(image_id, "decompress: starting");

    let result: Result<DecompressionStats> = async {
        // ── 1. Download and parse manifest ────────────────────────────────
        let manifest_raw = storage.download_manifest(image_id).await?;
        let manifest = crate::manifest::Manifest::from_bytes(&manifest_raw)?;

        if manifest.header.version != MANIFEST_VERSION {
            return Err(crate::Error::Manifest(format!(
                "manifest version {} not supported (expected {MANIFEST_VERSION})",
                manifest.header.version
            )));
        }

        // ── 2. Download and extract patches archive once ──────────────────
        let archive_bytes = storage.download_patches(image_id).await?;
        let patch_map = if archive_bytes.is_empty() {
            HashMap::new()
        } else {
            extract_archive_fn(&archive_bytes, manifest.header.patches_compressed)?
        };

        // ── 3. Open base image, index base Fs-partition handles ───────────
        let base_open = if options.base_root.as_os_str().is_empty() || !options.base_root.exists() {
            None
        } else {
            Some(image_format.open(&options.base_root)?)
        };
        let base_fs_handles: HashMap<u32, FsHandle> = if let Some(ref b) = base_open {
            b.partitions()?
                .into_iter()
                .filter_map(|ph| match ph {
                    PartitionHandle::Fs(fh) => Some((fh.descriptor.number, fh)),
                    _ => None,
                })
                .collect()
        } else {
            HashMap::new()
        };

        // ── 4. Create output image ────────────────────────────────────────
        let output_open = image_format.create(output_root, &manifest.disk_layout)?;

        // ── 5. Build shared decompress context ────────────────────────────
        let ctx = DecompressContext {
            storage: Arc::clone(&storage),
            router: Arc::clone(&router),
            workers: options.workers,
            patch_map: Arc::new(patch_map),
        };

        let mut total_files: usize = 0;
        let mut patches_verified: usize = 0;
        let mut total_bytes: u64 = 0;

        // ── 6. For each partition: create_partition() → PartitionDecompressor ──
        for pm in &manifest.partitions {
            let mut output_ph = output_open.create_partition(pm)?;

            // Inject base mount fn into output Fs handle (mirrors compress).
            if let PartitionHandle::Fs(ref mut fsh) = output_ph {
                if let Some(base_fh) = base_fs_handles.get(&pm.descriptor.number) {
                    fsh.set_base(base_fh);
                }
            }

            let decompressor: Box<dyn PartitionDecompressor> = match &pm.content {
                PartitionContent::Fs { .. } => Box::new(FsPartitionDecompressor),
                PartitionContent::BiosBoot { .. } => Box::new(BiosBootDecompressor),
                PartitionContent::MbrBootCode { .. } => Box::new(MbrDecompressor),
                PartitionContent::Raw { .. } => Box::new(RawPartitionDecompressor),
            };

            let part_stats = decompressor.decompress(&ctx, pm, &output_ph).await?;

            total_files += part_stats.files_written;
            patches_verified += part_stats.patches_verified;
            total_bytes += part_stats.bytes_written;
        }
        // output_open drops here → for qcow2: NbdConn drops → qemu-nbd --disconnect.

        let elapsed = started_at.elapsed();
        let decompression_stats = DecompressionStats {
            total_files,
            patches_verified,
            total_bytes,
            elapsed_secs: elapsed.as_secs_f64(),
        };
        info!(
            image_id,
            total_files,
            patches_verified,
            total_bytes,
            elapsed_secs = decompression_stats.elapsed_secs,
            "decompress: done"
        );
        Ok(decompression_stats)
    }
    .await;

    if let Err(ref e) = result {
        error!(image_id, error = %e, "decompress: failed, marking as Failed");
        let _ = storage
            .update_status(image_id, ImageStatus::Failed(e.to_string()))
            .await;
    }

    result
}
