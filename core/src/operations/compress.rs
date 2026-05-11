// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// operations::compress — free-function orchestrator for the compress pipeline

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use tracing::{error, info};

use super::{CompressOptions, CompressionStats};
use crate::compress::partitions::fs::stages::pack_archive::pack_and_upload_patches;
use crate::compress::partitions::{
    BiosBootCompressor, FsPartitionCompressor, MbrCompressor, PartitionCompressor,
    RawPartitionCompressor,
};
use crate::manifest::{
    Manifest, ManifestHeader, PartitionContent, PartitionManifest, Patch, MANIFEST_VERSION,
};
use crate::partitions::PartitionHandle;
use crate::storage::ImageStatus;
use crate::{Image, ImageMeta, Result, Storage};

/// Compress a target image against a base and upload the delta to storage.
pub async fn compress(
    image_format: Arc<dyn Image>,
    storage: Arc<dyn Storage>,
    router: Arc<crate::encoding::RouterEncoder>,
    source_root: &Path,
    target_root: &Path,
    options: CompressOptions,
) -> Result<CompressionStats> {
    let started_at = Instant::now();
    let image_id = &options.image_id;
    let base_image_id: Option<&str> = options.base_image_id.as_deref();

    info!(image_id, base_image_id, "compress: starting");

    // ── 0. Uniqueness check ───────────────────────────────────────────────
    if !options.overwrite {
        if let Some(existing) = storage.get_image(image_id).await? {
            if existing.status != "failed" {
                return Err(crate::Error::Other(format!(
                    "image '{image_id}' already exists (status: {}). \
                     Use --overwrite to replace it.",
                    existing.status
                )));
            }
        }
    }

    // ── 1. Register image and mark as compressing ─────────────────────────
    storage
        .register_image(&ImageMeta {
            image_id: image_id.clone(),
            base_image_id: options.base_image_id.clone(),
            format: image_format.format_name().to_string(),
            status: "pending".into(),
        })
        .await?;
    storage
        .update_status(image_id, ImageStatus::Compressing)
        .await?;

    // ── 2..7. Main pipeline (wrapped so any error triggers Failed status) ──
    let result: Result<CompressionStats> = async {
        // ── 2. Open target image ──────────────────────────────────────────
        let target_open = image_format.open(target_root)?;

        // ── 3. Open base image (if any) and index its Fs partitions ──────
        let _base_open: Option<Box<dyn crate::image::OpenImage>>;
        let base_fs_handles: HashMap<u32, crate::partitions::FsHandle>;
        if let Some(_base_id) = base_image_id {
            let open = image_format.open(source_root)?;
            let mut map = HashMap::new();
            for ph in open.partitions()? {
                if let PartitionHandle::Fs(fsh) = ph {
                    map.insert(fsh.descriptor.number, fsh);
                }
            }
            base_fs_handles = map;
            _base_open = Some(open);
        } else {
            base_fs_handles = HashMap::new();
            _base_open = None;
        }

        // ── 4. Process each target partition ──────────────────────────────
        let disk_layout = target_open.disk_layout().clone();
        let mut target_partitions = target_open.partitions()?;

        // Inject base mount fn into Fs handles so FsPartitionCompressor
        // doesn't need a global base_partitions map.
        for ph in &mut target_partitions {
            if let PartitionHandle::Fs(ref mut fsh) = ph {
                if let Some(base_fsh) = base_fs_handles.get(&fsh.descriptor.number) {
                    fsh.set_base(base_fsh);
                }
            }
        }

        // Timing sink shared across all Fs partitions.
        let timing_sink: std::sync::Arc<std::sync::Mutex<crate::operations::StageTimings>> =
            std::sync::Arc::new(std::sync::Mutex::new(
                crate::operations::StageTimings::default(),
            ));

        // Bytes uploaded to blob storage by BiosBoot / Raw / Mbr compressors.
        let mut non_fs_blobs_stored_bytes: u64 = 0;

        // Shared directory for all FS partition patch files.
        let all_patches_dir = tempfile::TempDir::new()?;

        let mut partition_manifests: Vec<PartitionManifest> = Vec::new();
        let mut _live_tmpdirs: Vec<tempfile::TempDir> = Vec::new();

        for target_ph in target_partitions {
            let descriptor = target_ph.descriptor().clone();
            info!(
                image_id,
                partition = descriptor.number,
                kind = ?std::mem::discriminant(&target_ph),
                "compress: processing partition"
            );

            let tmp_dir = tempfile::TempDir::new()?;
            let ctx = crate::compress::context::CompressContext {
                storage: Arc::clone(&storage),
                router: Arc::clone(&router),
                image_id: Arc::from(image_id.as_str()),
                base_image_id: options.base_image_id.as_deref().map(Arc::from),
                partition_number: Some(descriptor.number as i32),
                workers: options.workers,
                tmp_dir: Arc::from(tmp_dir.path()),
                patches_dir: Arc::from(all_patches_dir.path()),
                debug_dir: options.debug_dir.as_deref().map(Arc::from),
                timing_sink: Some(Arc::clone(&timing_sink)),
            };
            _live_tmpdirs.push(tmp_dir);

            let compressor: Box<dyn PartitionCompressor> = match &target_ph {
                PartitionHandle::Fs(_) => Box::new(FsPartitionCompressor),
                PartitionHandle::BiosBoot(_) => Box::new(BiosBootCompressor),
                PartitionHandle::Raw(_) => Box::new(RawPartitionCompressor),
                PartitionHandle::Mbr(_) => Box::new(MbrCompressor),
            };

            let (pm, part_blobs_stored) = compressor.compress(&ctx, target_ph).await?;
            non_fs_blobs_stored_bytes += part_blobs_stored;
            partition_manifests.push(pm);
        }

        // ── 5. Pack and upload the combined patches archive ───────────────
        let t_pack = std::time::Instant::now();
        let (archive_stored_bytes, patches_compressed) =
            pack_and_upload_patches(all_patches_dir.path(), storage.as_ref(), image_id).await?;
        if let Ok(mut guard) = timing_sink.lock() {
            guard.pack_archive_ms = t_pack.elapsed().as_millis() as u64;
        }

        // ── 6. Build and upload the manifest ──────────────────────────────
        let created_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let manifest = Manifest {
            header: ManifestHeader {
                version: MANIFEST_VERSION,
                image_id: image_id.clone(),
                base_image_id: options.base_image_id.clone(),
                format: image_format.format_name().to_string(),
                created_at,
                patches_compressed,
            },
            disk_layout,
            partitions: partition_manifests,
        };

        let manifest_bytes = rmp_serde::to_vec_named(&manifest)
            .map_err(|e| crate::Error::Manifest(e.to_string()))?;
        let manifest_bytes_gz = gzip_manifest(&manifest_bytes)?;
        info!(
            image_id,
            raw_bytes = manifest_bytes.len(),
            gz_bytes = manifest_bytes_gz.len(),
            "compress: uploading manifest (gzip)"
        );
        storage
            .upload_manifest(image_id, &manifest_bytes_gz)
            .await?;

        // ── 7. Mark as compressed ─────────────────────────────────────────
        storage
            .update_status(image_id, ImageStatus::Compressed)
            .await?;

        // ── 8. Compute stats from manifest ────────────────────────────────
        let elapsed = started_at.elapsed();
        let manifest_stored_bytes = manifest_bytes_gz.len() as u64;
        // non_fs_blobs_stored_bytes: blobs from BiosBoot/Raw/Mbr partitions.
        // Fs-partition blobs are tracked in PartitionContent::Fs.blobs_stored_bytes
        // and summed inside stats_from_manifest — no double-counting.
        let mut stats = stats_from_manifest(
            &manifest,
            elapsed,
            archive_stored_bytes + manifest_stored_bytes + non_fs_blobs_stored_bytes,
        );
        stats.stage_timings = timing_sink
            .lock()
            .ok()
            .map(|g| g.clone() as crate::operations::StageTimings);

        info!(
            image_id,
            added = stats.files_added,
            patched = stats.files_patched,
            removed = stats.files_removed,
            renamed = stats.files_renamed,
            entities_added = stats.entities_added,
            entities_changed = stats.entities_changed,
            entities_removed = stats.entities_removed,
            entities_renamed = stats.entities_renamed,
            entities_in_base = stats.entities_in_base,
            entities_in_target = stats.entities_in_target,
            blobs_stored_bytes =
                stats.total_stored_bytes - archive_stored_bytes - manifest_stored_bytes,
            archive_stored_bytes,
            manifest_stored_bytes,
            total_stored_bytes = stats.total_stored_bytes,
            elapsed_secs = stats.elapsed_secs,
            "compress: done"
        );

        Ok(stats)
    }
    .await;

    // ── 9. On any error — mark image as Failed ────────────────────────────
    if let Err(ref e) = result {
        error!(image_id, error = %e, "compress: failed, marking as Failed");
        let _ = storage
            .update_status(image_id, ImageStatus::Failed(e.to_string()))
            .await;
    }

    result
}

// ── Private helpers ───────────────────────────────────────────────────────────

fn gzip_manifest(bytes: &[u8]) -> Result<Vec<u8>> {
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;

    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    enc.write_all(bytes)
        .map_err(|e| crate::Error::Manifest(format!("gzip manifest write: {e}")))?;
    enc.finish()
        .map_err(|e| crate::Error::Manifest(format!("gzip manifest finish: {e}")))
}

fn stats_from_manifest(
    manifest: &Manifest,
    elapsed: std::time::Duration,
    stored_bytes: u64,
) -> CompressionStats {
    let mut stats = CompressionStats {
        elapsed_secs: elapsed.as_secs_f64(),
        total_stored_bytes: stored_bytes,
        ..CompressionStats::default()
    };
    for pm in &manifest.partitions {
        if let PartitionContent::Fs {
            records,
            base_entity_count,
            target_entity_count,
            blobs_stored_bytes,
            ..
        } = &pm.content
        {
            stats.total_stored_bytes += blobs_stored_bytes;
            stats.entities_in_base += base_entity_count;
            stats.entities_in_target += target_entity_count;
            for r in records {
                // ── Legacy file-only counters ─────────────────────────────────
                if !matches!(r.entry_type, crate::manifest::EntryType::Directory) {
                    match (&r.old_path, &r.new_path) {
                        (None, Some(_)) => stats.files_added += 1,
                        (Some(_), None) => stats.files_removed += 1,
                        (Some(old), Some(new)) if old != new => stats.files_renamed += 1,
                        (Some(_), Some(_)) => {
                            if matches!(r.patch, Some(Patch::Real(_))) {
                                stats.files_patched += 1;
                            }
                        }
                        _ => {}
                    }
                }
                // ── New entity counters (all types) ───────────────────────────
                match (&r.old_path, &r.new_path) {
                    (None, Some(_)) => stats.entities_added += 1,
                    (Some(_), None) => stats.entities_removed += 1,
                    (Some(old), Some(new)) if old != new => stats.entities_renamed += 1,
                    (Some(_), Some(_)) => stats.entities_changed += 1,
                    _ => {}
                }
                stats.total_source_bytes += r.size;
            }
        }
    }
    stats
}
