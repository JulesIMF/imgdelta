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
use tracing::{error, info};

use crate::compress::partitions::{
    BiosBootCompressor, FsPartitionCompressor, MbrCompressor, PartitionCompressor,
    RawPartitionCompressor,
};
use crate::manifest::{
    Data, Manifest, ManifestHeader, PartitionContent, PartitionManifest, MANIFEST_VERSION,
};
use crate::partition::PartitionKind;
use crate::partitions::PartitionHandle;
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

#[derive(Debug, Clone, Default)]
pub struct DeleteStats {
    /// Number of blob objects removed from storage.
    pub blobs_deleted: usize,
    /// Number of blobs skipped because they are still referenced by another image.
    pub blobs_kept: usize,
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
    /// If set, each compress stage dumps a JSON snapshot of the draft into
    /// this directory as `<NN>_<stage>.json`.
    pub debug_dir: Option<std::path::PathBuf>,
}

pub struct DecompressOptions {
    pub image_id: String,
    pub base_root: PathBuf,
    pub workers: usize,
}

pub struct DeleteOptions {
    pub image_id: String,
    /// When `true`, print a plan but do not actually delete anything.
    pub dry_run: bool,
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

    /// Delete a stored image and all data exclusively owned by it.
    ///
    /// Shared blobs (referenced by other images) are left intact.
    /// Safe ordering: blob_origins → patches → manifest → image_meta.
    async fn delete_image(&self, options: DeleteOptions) -> Result<DeleteStats>;
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
            let mut _live_mounts: Vec<Box<dyn crate::partitions::MountHandle>> = Vec::new();
            let mut _live_tmpdirs: Vec<tempfile::TempDir> = Vec::new();

            for target_ph in target_partitions {
                let descriptor = target_ph.descriptor().clone();
                info!(
                    image_id,
                    partition = descriptor.number,
                    kind = ?std::mem::discriminant(&target_ph),
                    "compress: processing partition"
                );

                let ctx = crate::compress::partitions::fs::context::StageContext {
                    storage: Arc::clone(&self.storage),
                    router: Arc::clone(&self.router),
                    image_id: Arc::from(image_id.as_str()),
                    base_image_id: options.base_image_id.as_deref().map(Arc::from),
                    partition_number: Some(descriptor.number as i32),
                    workers: options.workers,
                    tmp_dir: {
                        let t = tempfile::TempDir::new()?;
                        let p: Arc<std::path::Path> = Arc::from(t.path());
                        _live_tmpdirs.push(t);
                        p
                    },
                    debug_dir: options.debug_dir.as_deref().map(Arc::from),
                };

                let fs_type = match &descriptor.kind {
                    PartitionKind::Fs { fs_type } => fs_type.clone(),
                    _ => "unknown".into(),
                };

                let compressor: Box<dyn PartitionCompressor> = match &target_ph {
                    PartitionHandle::Fs(_) => Box::new(FsPartitionCompressor),
                    PartitionHandle::BiosBoot(_) => Box::new(BiosBootCompressor),
                    PartitionHandle::Raw(_) => Box::new(RawPartitionCompressor),
                    PartitionHandle::Mbr(_) => Box::new(MbrCompressor),
                };

                let (pm, compressed, archive_bytes) = compressor
                    .compress(
                        &ctx,
                        target_ph,
                        &fs_type,
                        &base_partitions,
                        &mut _live_mounts,
                        &mut _live_tmpdirs,
                    )
                    .await?;

                patches_compressed = compressed;
                archive_stored_bytes += archive_bytes;
                partition_manifests.push(pm);
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
        use crate::decompress::{
            BiosBootDecompressor, FsPartitionDecompressor, MbrDecompressor, PartitionDecompressor,
            RawPartitionDecompressor,
        };
        use crate::manifest::{PartitionContent, MANIFEST_VERSION};
        use crate::partitions::FsHandle;
        use std::collections::HashMap;
        use tempfile::TempDir;

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

            // ── 2. Chain detection ────────────────────────────────────────────
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

            // ── 3. Download patches archive (once for all partitions) ─────────
            let archive_bytes = self.storage.download_patches(image_id).await?;

            // ── 4. Open base image via the same driver — completely format-agnostic.
            let base_open: Option<Box<dyn crate::image::OpenImage>> =
                if options.base_root.as_os_str().is_empty() || !options.base_root.exists() {
                    None
                } else {
                    Some(self.image_format.open(&options.base_root)?)
                };
            // Index base Fs-partition handles by partition number.
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

            // ── 5. Create output image — format-agnostic via Image::create(). ─
            //   • qcow2  → qemu-img create + NBD RW + sgdisk (write GPT)
            //   • directory → create_dir_all(output_root)
            let output_open = self
                .image_format
                .create(output_root, &manifest.disk_layout)?;

            let patches_compressed = manifest.header.patches_compressed;
            let mut total_files: usize = 0;
            let mut patches_verified: usize = 0;
            let mut total_bytes: u64 = 0;

            // ── 6. For each partition: create_partition() → PartitionDecompressor ──
            for pm in &manifest.partitions {
                // For Fs partitions we need a mounted base directory.
                // Binary partitions receive an empty path and ignore it.
                let _base_mount: Option<Box<dyn crate::partitions::MountHandle>>;
                let _base_tmpdir: Option<TempDir>;
                let base_root: std::path::PathBuf =
                    if matches!(&pm.content, PartitionContent::Fs { .. }) {
                        if let Some(fh) = base_fs_handles.get(&pm.descriptor.number) {
                            let mount = fh.mount()?;
                            let path = mount.root().to_path_buf();
                            _base_mount = Some(mount);
                            _base_tmpdir = None;
                            path
                        } else {
                            let dir = TempDir::new()
                                .map_err(|e| crate::Error::Other(format!("TempDir: {e}")))?;
                            let path = dir.path().to_path_buf();
                            _base_mount = None;
                            _base_tmpdir = Some(dir);
                            path
                        }
                    } else {
                        _base_mount = None;
                        _base_tmpdir = None;
                        std::path::PathBuf::new()
                    };

                // create_partition prepares the partition (mkfs for Fs, noop for
                // binary types) and returns a writable handle.
                let output_ph = output_open.create_partition(pm)?;

                let decompressor: Box<dyn PartitionDecompressor> = match &pm.content {
                    PartitionContent::Fs { .. } => Box::new(FsPartitionDecompressor),
                    PartitionContent::BiosBoot { .. } => Box::new(BiosBootDecompressor),
                    PartitionContent::MbrBootCode { .. } => Box::new(MbrDecompressor),
                    PartitionContent::Raw { .. } => Box::new(RawPartitionDecompressor),
                };

                let part_stats = decompressor
                    .decompress(
                        pm,
                        &base_root,
                        &output_ph,
                        Arc::clone(&self.storage),
                        &archive_bytes,
                        patches_compressed,
                        Arc::clone(&self.router),
                        options.workers,
                    )
                    .await?;
                // _base_mount / _base_tmpdir drop here → umount / tmpdir cleanup.

                total_files += part_stats.files_written;
                patches_verified += part_stats.patches_verified;
                total_bytes += part_stats.bytes_written;
            }
            // output_open drops here → for qcow2: NbdConn drops → qemu-nbd --disconnect.

            // ── 7. Update status ───────────────────────────────────────────────
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

        if let Err(ref e) = result {
            error!(image_id, error = %e, "decompress: failed, marking as Failed");
            let _ = self
                .storage
                .update_status(image_id, ImageStatus::Failed(e.to_string()))
                .await;
        }

        result
    }

    async fn delete_image(&self, options: DeleteOptions) -> Result<DeleteStats> {
        let image_id = &options.image_id;

        // 1. Verify image exists.
        self.storage
            .get_image(image_id)
            .await?
            .ok_or_else(|| crate::Error::Other(format!("image not found: {image_id}")))?;

        // 2. Refuse to delete if any other image uses this one as a base.
        let all_images = self.storage.list_images().await?;
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
        let manifest_bytes = self.storage.download_manifest(image_id).await?;
        let this_manifest = Manifest::from_bytes(&manifest_bytes)
            .map_err(|e| crate::Error::Other(format!("manifest decode: {e}")))?;
        let this_blobs = collect_manifest_blob_ids(&this_manifest);

        // 4. Collect blob IDs referenced by ALL OTHER images.
        let mut in_use_blobs = std::collections::HashSet::<uuid::Uuid>::new();
        for other in &all_images {
            if other.image_id == *image_id {
                continue;
            }
            match self.storage.download_manifest(&other.image_id).await {
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
                self.storage.delete_blob(*blob_id).await?;
                tracing::debug!(%blob_id, "delete_image: blob deleted");
                stats.blobs_deleted += 1;
            }
        }

        if !options.dry_run {
            // 6. Remove blob_origins rows for this image.
            self.storage.delete_blob_origins(image_id).await?;
            // 7. Remove patches archive.
            self.storage.delete_patches(image_id).await?;
            // 8. Remove manifest.
            self.storage.delete_manifest(image_id).await?;
            // 9. Remove image metadata record.
            self.storage.delete_image_meta(image_id).await?;
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
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Collect all blob UUIDs referenced anywhere in `manifest`.
fn collect_manifest_blob_ids(manifest: &Manifest) -> std::collections::HashSet<uuid::Uuid> {
    let mut ids = std::collections::HashSet::new();
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
                    // Patch::Real entries are stored inside the patches archive,
                    // not as individual blob objects — nothing to collect here.
                }
            }
        }
    }
    ids
}

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
                // Add blob bytes to stored_bytes so the ratio is accurate.
                if let Some(crate::manifest::Data::BlobRef(b)) = &r.data {
                    stats.total_stored_bytes += b.size;
                }
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
