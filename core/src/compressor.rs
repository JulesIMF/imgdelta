// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// DefaultCompressor orchestrator: coordinates the full compress/decompress pipeline

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use tracing::{error, info};

use crate::compress_pipeline::compress_fs_partition;
use crate::image::PartitionHandle;
use crate::manifest::{
    BlobRef, Manifest, ManifestHeader, PartitionContent, PartitionManifest, MANIFEST_VERSION,
};
use crate::partition::PartitionKind;
use crate::storage::ImageStatus;
use crate::{Image, ImageMeta, Result, Storage};

// ── Stats ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct CompressionStats {
    pub files_patched: usize,
    pub files_added: usize,
    pub files_removed: usize,
    pub files_renamed: usize,
    pub total_source_bytes: u64,
    pub total_stored_bytes: u64,
    pub elapsed_secs: f64,
}

impl CompressionStats {
    pub fn ratio(&self) -> f64 {
        if self.total_source_bytes == 0 {
            return 1.0;
        }
        self.total_stored_bytes as f64 / self.total_source_bytes as f64
    }
}

#[derive(Debug, Clone, Default)]
pub struct DecompressionStats {
    pub total_files: usize,
    pub patches_verified: usize,
    pub total_bytes: u64,
    pub elapsed_secs: f64,
}

// ── Options ───────────────────────────────────────────────────────────────────

pub struct CompressOptions {
    pub image_id: String,
    pub base_image_id: Option<String>,
    pub workers: usize,
    pub passthrough_threshold: f64,
    /// When `true`, a pre-existing image with the same `image_id` is silently
    /// overwritten.  When `false` (the default), an error is returned if the
    /// image already exists and is not in the `failed` state.
    pub overwrite: bool,
}

pub struct DecompressOptions {
    pub image_id: String,
    pub base_root: PathBuf,
    pub workers: usize,
}

// ── Trait ─────────────────────────────────────────────────────────────────────

#[async_trait]
pub trait Compressor: Send + Sync {
    async fn compress(
        &self,
        source_root: &Path,
        target_root: &Path,
        options: CompressOptions,
    ) -> Result<CompressionStats>;

    async fn decompress(
        &self,
        output_root: &Path,
        options: DecompressOptions,
    ) -> Result<DecompressionStats>;
}

// ── DefaultCompressor ─────────────────────────────────────────────────────────

pub struct DefaultCompressor {
    storage: Arc<dyn Storage>,
    router: Arc<crate::routing::RouterEncoder>,
    image_format: Arc<dyn Image>,
}

impl DefaultCompressor {
    /// Create a `DefaultCompressor` with a fully configured router.
    pub fn new(
        image_format: Arc<dyn Image>,
        storage: Arc<dyn Storage>,
        router: Arc<crate::routing::RouterEncoder>,
    ) -> Self {
        Self {
            image_format,
            storage,
            router,
        }
    }

    /// Create a `DefaultCompressor` with a single catch-all encoder.
    pub fn with_encoder(
        image_format: Arc<dyn Image>,
        storage: Arc<dyn Storage>,
        encoder: Arc<dyn crate::encoder::PatchEncoder>,
    ) -> Self {
        Self::new(
            image_format,
            storage,
            Arc::new(crate::routing::RouterEncoder::new(vec![], encoder)),
        )
    }
}

#[async_trait]
impl Compressor for DefaultCompressor {
    async fn compress(
        &self,
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
            if let Some(existing) = self.storage.get_image(image_id).await? {
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
        self.storage
            .register_image(&ImageMeta {
                image_id: image_id.clone(),
                base_image_id: options.base_image_id.clone(),
                format: self.image_format.format_name().to_string(),
                status: "pending".into(),
            })
            .await?;
        self.storage
            .update_status(image_id, ImageStatus::Compressing)
            .await?;

        // ── 2..7. Main pipeline (wrapped so any error triggers Failed status) ──
        let result: Result<CompressionStats> = async {
            // ── 2. Open target image ───────────────────────────────────────────────
            let target_open = self.image_format.open(target_root)?;

            // ── 3. Open base image (if any) and index its partitions by number ────
            //
            // `_base_open` is kept alive so that any mount handles derived from it
            // stay valid during partition processing.  It must outlive
            // `base_partitions`.
            let _base_open: Option<Box<dyn crate::image::OpenImage>>;
            let base_partitions: HashMap<u32, PartitionHandle>;
            if let Some(_base_id) = base_image_id {
                let open = self.image_format.open(source_root)?;
                let mut map = HashMap::new();
                for ph in open.partitions()? {
                    map.insert(ph.descriptor().number, ph);
                }
                base_partitions = map;
                _base_open = Some(open);
            } else {
                base_partitions = HashMap::new();
                _base_open = None;
            }

            // ── 4. Process each target partition ──────────────────────────────────
            let disk_layout = target_open.disk_layout().clone();
            let target_partitions = target_open.partitions()?;

            let mut partition_manifests: Vec<PartitionManifest> = Vec::new();
            let mut patches_compressed = false;
            let mut archive_stored_bytes: u64 = 0;
            // Keep TempDirs and MountHandles alive until all processing is done.
            let mut _live_mounts: Vec<Box<dyn crate::image::MountHandle>> = Vec::new();
            let mut _live_tmpdirs: Vec<tempfile::TempDir> = Vec::new();

            for target_ph in target_partitions {
                match target_ph {
                    PartitionHandle::Fs(fs_handle) => {
                        let descriptor = fs_handle.descriptor.clone();
                        info!(
                            image_id,
                            partition = descriptor.number,
                            "compress: processing Fs partition"
                        );

                        // Mount target partition.
                        let target_mount = fs_handle.mount()?;
                        let target_root_path: PathBuf = target_mount.root().to_path_buf();
                        _live_mounts.push(target_mount);

                        // Find matching base Fs partition, or create empty temp dir.
                        let base_root_path: PathBuf = match base_partitions.get(&descriptor.number)
                        {
                            Some(PartitionHandle::Fs(base_fs)) => {
                                let base_mount = base_fs.mount()?;
                                let p = base_mount.root().to_path_buf();
                                _live_mounts.push(base_mount);
                                p
                            }
                            _ => {
                                // No matching base — first compression or type mismatch.
                                let tmp = tempfile::TempDir::new()?;
                                let p = tmp.path().to_path_buf();
                                _live_tmpdirs.push(tmp);
                                p
                            }
                        };

                        let fs_type = match &descriptor.kind {
                            PartitionKind::Fs { fs_type } => fs_type.clone(),
                            _ => "unknown".into(),
                        };

                        let (pm, compressed, archive_bytes) = compress_fs_partition(
                            &base_root_path,
                            &target_root_path,
                            &descriptor,
                            self.storage.as_ref(),
                            image_id,
                            base_image_id,
                            &self.router,
                            &fs_type,
                            options.workers,
                        )
                        .await?;

                        patches_compressed = compressed;
                        archive_stored_bytes += archive_bytes;
                        partition_manifests.push(pm);
                    }

                    PartitionHandle::BiosBoot(bb_handle) => {
                        let descriptor = bb_handle.descriptor.clone();
                        let bytes = bb_handle.read_raw()?;
                        let sha256 = hex::encode(Sha256::digest(&bytes));
                        let size = bytes.len() as u64;
                        let blob_id = match self.storage.blob_exists(&sha256).await? {
                            Some(id) => id,
                            None => self.storage.upload_blob(&sha256, &bytes).await?,
                        };
                        partition_manifests.push(PartitionManifest {
                            descriptor,
                            content: PartitionContent::BiosBoot {
                                blob_id,
                                sha256,
                                size,
                            },
                        });
                    }

                    PartitionHandle::Raw(raw_handle) => {
                        let descriptor = raw_handle.descriptor.clone();
                        let bytes = raw_handle.read_raw()?;
                        let sha256 = hex::encode(Sha256::digest(&bytes));
                        let size = bytes.len() as u64;
                        let blob_id = match self.storage.blob_exists(&sha256).await? {
                            Some(id) => id,
                            None => self.storage.upload_blob(&sha256, &bytes).await?,
                        };
                        partition_manifests.push(PartitionManifest {
                            descriptor,
                            content: PartitionContent::Raw {
                                size,
                                blob: Some(BlobRef { blob_id, size }),
                                patch: None,
                            },
                        });
                    }
                }
            }

            // ── 5. Build and upload the manifest ──────────────────────────────────
            let created_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);

            let manifest = Manifest {
                header: ManifestHeader {
                    version: MANIFEST_VERSION,
                    image_id: image_id.clone(),
                    base_image_id: options.base_image_id.clone(),
                    format: self.image_format.format_name().to_string(),
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
            self.storage
                .upload_manifest(image_id, &manifest_bytes_gz)
                .await?;

            // ── 6. Mark as compressed ─────────────────────────────────────────────
            self.storage
                .update_status(image_id, ImageStatus::Compressed)
                .await?;

            // ── 7. Compute stats from manifest ────────────────────────────────────
            let elapsed = started_at.elapsed();
            let manifest_stored_bytes = manifest_bytes_gz.len() as u64;
            let stats = stats_from_manifest(
                &manifest,
                elapsed,
                archive_stored_bytes + manifest_stored_bytes,
            );

            info!(
                image_id,
                added = stats.files_added,
                patched = stats.files_patched,
                removed = stats.files_removed,
                renamed = stats.files_renamed,
                archive_stored_bytes,
                manifest_stored_bytes,
                total_stored_bytes = stats.total_stored_bytes,
                elapsed_secs = stats.elapsed_secs,
                "compress: done"
            );

            Ok(stats)
        }
        .await; // end of main pipeline

        // ── 8. On any error — mark image as Failed ────────────────────────────
        if let Err(ref e) = result {
            error!(image_id, error = %e, "compress: failed, marking as Failed");
            let _ = self
                .storage
                .update_status(image_id, ImageStatus::Failed(e.to_string()))
                .await;
        }

        result
    }

    async fn decompress(
        &self,
        output_root: &Path,
        options: DecompressOptions,
    ) -> Result<DecompressionStats> {
        use crate::decompress_pipeline::decompress_fs_partition;
        use crate::manifest::MANIFEST_VERSION;

        let started_at = Instant::now();
        let image_id = &options.image_id;

        info!(image_id, "decompress: starting");

        let result: Result<DecompressionStats> = async {
            // ── 1. Download and parse manifest ────────────────────────────────
            let manifest_raw = self.storage.download_manifest(image_id).await?;
            let manifest = crate::manifest::Manifest::from_bytes(&manifest_raw)?;

            if manifest.header.version != MANIFEST_VERSION {
                return Err(crate::Error::Manifest(format!(
                    "manifest version {} not supported (expected {MANIFEST_VERSION})",
                    manifest.header.version
                )));
            }

            // ── 2. Chain detection — refuse to decompress if the base is itself
            //        a delta (chained decompression is not supported). ──────────
            if let Some(ref base_id) = manifest.header.base_image_id {
                if let Some(base_meta) = self.storage.get_image(base_id).await? {
                    if base_meta.base_image_id.is_some() {
                        return Err(crate::Error::Other(format!(
                            "chained decompression is not supported: \
                             '{image_id}' → '{base_id}' → '{}'. \
                             Decompress '{base_id}' first to obtain a full image, \
                             then decompress '{image_id}' against it.",
                            base_meta.base_image_id.as_deref().unwrap_or("?")
                        )));
                    }
                }
            }

            // ── 3. Download patches archive (once for all Fs partitions) ──────
            let archive_bytes = self.storage.download_patches(image_id).await?;
            let patches_compressed = manifest.header.patches_compressed;

            // ── 4. Process each partition ──────────────────────────────────────
            let mut total_files: usize = 0;
            let mut patches_verified: usize = 0;
            let mut total_bytes: u64 = 0;

            for pm in &manifest.partitions {
                match &pm.content {
                    PartitionContent::Fs {
                        records,
                        fs_type: _,
                    } => {
                        let part_stats = decompress_fs_partition(
                            &options.base_root,
                            output_root,
                            records,
                            &archive_bytes,
                            patches_compressed,
                            Arc::clone(&self.storage),
                            &self.router,
                            options.workers,
                        )
                        .await?;
                        total_files += part_stats.files_written;
                        patches_verified += part_stats.patches_verified;
                        total_bytes += part_stats.bytes_written;
                    }

                    PartitionContent::BiosBoot { blob_id, size, .. } => {
                        let data = self.storage.download_blob(*blob_id).await?;
                        let out_path =
                            output_root.join(format!("biosboot_{}.bin", pm.descriptor.number));
                        if let Some(p) = out_path.parent() {
                            std::fs::create_dir_all(p).map_err(|e| {
                                crate::Error::Other(format!("create_dir {}: {e}", p.display()))
                            })?;
                        }
                        std::fs::write(&out_path, &data).map_err(|e| {
                            crate::Error::Other(format!("write {}: {e}", out_path.display()))
                        })?;
                        total_files += 1;
                        total_bytes += *size;
                    }

                    PartitionContent::Raw { blob, size, .. } => {
                        if let Some(bref) = blob {
                            let data = self.storage.download_blob(bref.blob_id).await?;
                            let out_path = output_root
                                .join(format!("raw_partition_{}.img", pm.descriptor.number));
                            if let Some(p) = out_path.parent() {
                                std::fs::create_dir_all(p).map_err(|e| {
                                    crate::Error::Other(format!("create_dir {}: {e}", p.display()))
                                })?;
                            }
                            std::fs::write(&out_path, &data).map_err(|e| {
                                crate::Error::Other(format!("write {}: {e}", out_path.display()))
                            })?;
                            total_files += 1;
                            total_bytes += *size;
                        }
                    }
                }
            }

            // ── 5. Update status ───────────────────────────────────────────────
            self.storage
                .update_status(image_id, ImageStatus::Compressed)
                .await?;

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

        // On any error — mark image as Failed
        if let Err(ref e) = result {
            error!(image_id, error = %e, "decompress: failed, marking as Failed");
            let _ = self
                .storage
                .update_status(image_id, ImageStatus::Failed(e.to_string()))
                .await;
        }

        result
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Derive `CompressionStats` from the final manifest and wall-clock elapsed time.
///
/// File counts are computed from record structure:
/// - `files_added`   : `old_path = None, new_path = Some`
/// - `files_removed` : `old_path = Some, new_path = None`
/// - `files_patched` : `old_path = Some, new_path = Some, patch = Some(Real)`
///
/// `total_source_bytes` is the sum of `record.size` across all records.
/// `total_stored_bytes` is the size of uploaded patches archive plus the manifest.
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
        if let PartitionContent::Fs { records, .. } = &pm.content {
            for r in records {
                // Directory records are infrastructure changes; exclude them from
                // file-level counters so callers get meaningful file statistics.
                if matches!(r.entry_type, crate::manifest::EntryType::Directory) {
                    continue;
                }
                match (&r.old_path, &r.new_path) {
                    (None, Some(_)) => stats.files_added += 1,
                    (Some(_), None) => stats.files_removed += 1,
                    (Some(old), Some(new)) if old != new => stats.files_renamed += 1,
                    (Some(_), Some(_)) => {
                        if matches!(r.patch, Some(crate::manifest::Patch::Real(_))) {
                            stats.files_patched += 1;
                        }
                    }
                    _ => {}
                }
                stats.total_source_bytes += r.size;
            }
        }
    }
    stats
}

// ── Manifest gzip helper ─────────────────────────────────────────────────────

/// Gzip-compress manifest bytes for storage.
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
