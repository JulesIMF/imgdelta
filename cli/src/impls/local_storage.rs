// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// LocalStorage: filesystem-backed Storage implementation for local testing

/// File-based [`Storage`] implementation for local testing and single-machine use.
///
/// No S3 or PostgreSQL required.  All data lives under a single `base_dir`:
///
/// ```text
/// {base_dir}/
///   blobs/{uuid}                    — raw blob bytes, keyed by UUID
///   images/{image_id}/manifest      — serialised Manifest bytes
///   images/{image_id}/patches.tar   — patches tar (or tar.gz)
///   images/{image_id}/meta.json     — ImageMeta + ImageStatus
///   sha256_index.json               — sha256 hex → {uuid, compressed} mapping (dedup)
///   blobs.json                      — [(uuid, image_id, file_path)] blob origins
/// ```
///
/// Blob UUIDs are derived deterministically from their SHA-256 via UUID v5
/// (namespace OID), so `upload_blob` is idempotent at the byte level.
///
/// Thread safety: all mutable state is protected by a [`Mutex`].
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use image_delta_core::storage::{BlobCandidate, ImageMeta, ImageStatus, Storage};
use image_delta_core::{Error, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Persisted form of image metadata (includes status).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredImageMeta {
    pub image_id: String,
    pub base_image_id: Option<String>,
    pub format: String,
    pub status: String,
}

impl From<&ImageMeta> for StoredImageMeta {
    fn from(m: &ImageMeta) -> Self {
        Self {
            image_id: m.image_id.clone(),
            base_image_id: m.base_image_id.clone(),
            format: m.format.clone(),
            status: m.status.clone(),
        }
    }
}

impl From<StoredImageMeta> for ImageMeta {
    fn from(s: StoredImageMeta) -> Self {
        Self {
            image_id: s.image_id,
            base_image_id: s.base_image_id,
            format: s.format,
            status: s.status,
        }
    }
}

/// Value stored in `sha256_index.json` for each known blob.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct BlobIndexEntry {
    pub uuid: Uuid,
    /// `true` if the blob file on disk is gzip-compressed.
    #[serde(default)]
    pub compressed: bool,
}

/// Persisted blob-origin record (`blobs.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct BlobOriginRecord {
    pub blob_uuid: String,
    pub orig_image_id: String,
    pub base_image_id: Option<String>,
    pub file_path: String,
}

/// In-memory cache of mutable index state (persisted to disk after each write).
#[derive(Debug, Default)]
struct LocalStorageInner {
    /// sha256 hex → BlobIndexEntry { uuid, compressed }
    sha256_index: HashMap<String, BlobIndexEntry>,
    /// blob origins — persisted as `blobs.json`
    blob_origins: Vec<BlobOriginRecord>,
    /// UUIDs of compressed blobs — derived from `sha256_index` at load time,
    /// never persisted separately.
    compressed_blobs: HashSet<Uuid>,
}

pub struct LocalStorage {
    base_dir: PathBuf,
    inner: Mutex<LocalStorageInner>,
}

impl LocalStorage {
    /// Create a new `LocalStorage` rooted at `base_dir`.
    ///
    /// The directory (and required sub-directories) are created if they do not
    /// already exist.  Any existing index files are loaded into memory.
    pub fn new(base_dir: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let base_dir = base_dir.into();
        std::fs::create_dir_all(base_dir.join("blobs"))
            .map_err(|e| anyhow::anyhow!("cannot create blobs dir: {e}"))?;
        std::fs::create_dir_all(base_dir.join("images"))
            .map_err(|e| anyhow::anyhow!("cannot create images dir: {e}"))?;

        let sha256_index = Self::load_sha256_index(&base_dir);
        // Derive compressed_blobs from the index — no separate file needed.
        let compressed_blobs = sha256_index
            .values()
            .filter(|e| e.compressed)
            .map(|e| e.uuid)
            .collect();
        let blob_origins = Self::load_blob_origins(&base_dir);

        Ok(Self {
            base_dir,
            inner: Mutex::new(LocalStorageInner {
                sha256_index,
                blob_origins,
                compressed_blobs,
            }),
        })
    }

    // ── Paths ─────────────────────────────────────────────────────────────────

    fn blob_path(&self, uuid: Uuid) -> PathBuf {
        self.base_dir.join("blobs").join(uuid.to_string())
    }

    fn image_dir(&self, image_id: &str) -> PathBuf {
        self.base_dir.join("images").join(image_id)
    }

    fn manifest_path(&self, image_id: &str) -> PathBuf {
        self.image_dir(image_id).join("manifest")
    }

    fn patches_path(&self, image_id: &str) -> PathBuf {
        self.image_dir(image_id).join("patches.tar")
    }

    fn meta_path(&self, image_id: &str) -> PathBuf {
        self.image_dir(image_id).join("meta.json")
    }

    // ── Index I/O ─────────────────────────────────────────────────────────────

    fn load_sha256_index(base_dir: &Path) -> HashMap<String, BlobIndexEntry> {
        let path = base_dir.join("sha256_index.json");
        let Ok(bytes) = std::fs::read(&path) else {
            return HashMap::new();
        };
        // Try new format {sha256: {uuid, compressed}} first; fall back to legacy
        // {sha256: uuid_str} so old stores are migrated transparently.
        if let Ok(new) = serde_json::from_slice::<HashMap<String, BlobIndexEntry>>(&bytes) {
            return new;
        }
        serde_json::from_slice::<HashMap<String, String>>(&bytes)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(k, v)| {
                v.parse::<Uuid>().ok().map(|uuid| {
                    (
                        k,
                        BlobIndexEntry {
                            uuid,
                            compressed: false,
                        },
                    )
                })
            })
            .collect()
    }

    fn save_sha256_index(
        base_dir: &Path,
        index: &HashMap<String, BlobIndexEntry>,
    ) -> std::io::Result<()> {
        let bytes = serde_json::to_vec_pretty(index).map_err(std::io::Error::other)?;
        std::fs::write(base_dir.join("sha256_index.json"), bytes)
    }

    fn load_blob_origins(base_dir: &Path) -> Vec<BlobOriginRecord> {
        // Support both old name (blob_origins.json) and new name (blobs.json).
        let new_path = base_dir.join("blobs.json");
        let old_path = base_dir.join("blob_origins.json");
        let path = if new_path.exists() {
            new_path
        } else {
            old_path
        };
        let Ok(bytes) = std::fs::read(&path) else {
            return Vec::new();
        };
        serde_json::from_slice(&bytes).unwrap_or_default()
    }

    fn save_blob_origins(base_dir: &Path, origins: &[BlobOriginRecord]) -> std::io::Result<()> {
        let bytes = serde_json::to_vec_pretty(origins).map_err(std::io::Error::other)?;
        std::fs::write(base_dir.join("blobs.json"), bytes)
    }

    /// Try gzip-compress `data`; return (bytes_to_store, was_compressed).
    fn try_compress_blob(data: &[u8]) -> (Vec<u8>, bool) {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;

        let mut enc = GzEncoder::new(Vec::with_capacity(data.len()), Compression::default());
        if enc.write_all(data).is_err() {
            return (data.to_vec(), false);
        }
        match enc.finish() {
            Ok(gz) if gz.len() < data.len() => (gz, true),
            _ => (data.to_vec(), false),
        }
    }

    /// Decompress a gzip blob back to raw bytes.
    fn decompress_blob(data: &[u8]) -> Result<Vec<u8>> {
        use flate2::read::GzDecoder;
        use std::io::Read;

        let mut dec = GzDecoder::new(data);
        let mut out = Vec::new();
        dec.read_to_end(&mut out)
            .map_err(|e| Error::Storage(format!("decompress blob: {e}")))?;
        Ok(out)
    }

    fn io_err(e: impl std::fmt::Display) -> Error {
        Error::Storage(e.to_string())
    }
}

#[async_trait]
impl Storage for LocalStorage {
    async fn blob_exists(&self, sha256: &str) -> Result<Option<Uuid>> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .sha256_index
            .get(sha256)
            .map(|e| e.uuid))
    }

    async fn upload_blob(&self, sha256: &str, data: &[u8]) -> Result<Uuid> {
        let mut inner = self.inner.lock().unwrap();

        // Idempotent: if already uploaded, return existing UUID.
        if let Some(existing) = inner.sha256_index.get(sha256) {
            return Ok(existing.uuid);
        }

        // Derive a deterministic UUID from the SHA-256.
        let uuid = Uuid::new_v5(&Uuid::NAMESPACE_OID, sha256.as_bytes());
        let path = self.blob_path(uuid);

        let (bytes_to_store, compressed) = Self::try_compress_blob(data);
        std::fs::write(&path, &bytes_to_store).map_err(Self::io_err)?;

        if compressed {
            inner.compressed_blobs.insert(uuid);
        }

        inner
            .sha256_index
            .insert(sha256.to_string(), BlobIndexEntry { uuid, compressed });
        Self::save_sha256_index(&self.base_dir, &inner.sha256_index).map_err(Self::io_err)?;
        Ok(uuid)
    }

    async fn download_blob(&self, blob_id: Uuid) -> Result<Vec<u8>> {
        let raw = std::fs::read(self.blob_path(blob_id))
            .map_err(|e| Error::Storage(format!("blob {blob_id} not found: {e}")))?;
        let is_compressed = self
            .inner
            .lock()
            .unwrap()
            .compressed_blobs
            .contains(&blob_id);
        if is_compressed {
            Self::decompress_blob(&raw)
        } else {
            Ok(raw)
        }
    }

    async fn upload_manifest(&self, image_id: &str, manifest_bytes: &[u8]) -> Result<()> {
        std::fs::create_dir_all(self.image_dir(image_id)).map_err(Self::io_err)?;
        std::fs::write(self.manifest_path(image_id), manifest_bytes).map_err(Self::io_err)
    }

    async fn download_manifest(&self, image_id: &str) -> Result<Vec<u8>> {
        std::fs::read(self.manifest_path(image_id))
            .map_err(|e| Error::Storage(format!("manifest for {image_id} not found: {e}")))
    }

    async fn upload_patches(&self, image_id: &str, data: &[u8], _compressed: bool) -> Result<()> {
        std::fs::create_dir_all(self.image_dir(image_id)).map_err(Self::io_err)?;
        std::fs::write(self.patches_path(image_id), data).map_err(Self::io_err)
    }

    async fn download_patches(&self, image_id: &str) -> Result<Vec<u8>> {
        std::fs::read(self.patches_path(image_id))
            .map_err(|e| Error::Storage(format!("patches for {image_id} not found: {e}")))
    }

    async fn register_image(&self, meta: &ImageMeta) -> Result<()> {
        let dir = self.image_dir(&meta.image_id);
        std::fs::create_dir_all(&dir).map_err(Self::io_err)?;
        let stored = StoredImageMeta::from(meta);
        let bytes =
            serde_json::to_vec_pretty(&stored).map_err(|e| Error::Storage(e.to_string()))?;
        std::fs::write(self.meta_path(&meta.image_id), bytes).map_err(Self::io_err)
    }

    async fn get_image(&self, image_id: &str) -> Result<Option<ImageMeta>> {
        let path = self.meta_path(image_id);
        if !path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(&path).map_err(Self::io_err)?;
        let stored: StoredImageMeta = serde_json::from_slice(&bytes)
            .map_err(|e| Error::Storage(format!("corrupt meta for {image_id}: {e}")))?;
        Ok(Some(ImageMeta::from(stored)))
    }

    async fn update_status(&self, image_id: &str, status: ImageStatus) -> Result<()> {
        let path = self.meta_path(image_id);
        let bytes = std::fs::read(&path)
            .map_err(|e| Error::Storage(format!("image {image_id} not found: {e}")))?;
        let mut stored: StoredImageMeta = serde_json::from_slice(&bytes)
            .map_err(|e| Error::Storage(format!("corrupt meta for {image_id}: {e}")))?;
        stored.status = format!("{status:?}").to_lowercase();
        let updated =
            serde_json::to_vec_pretty(&stored).map_err(|e| Error::Storage(e.to_string()))?;
        std::fs::write(&path, updated).map_err(Self::io_err)
    }

    async fn list_images(&self) -> Result<Vec<ImageMeta>> {
        let images_dir = self.base_dir.join("images");
        if !images_dir.exists() {
            return Ok(Vec::new());
        }
        let mut result = Vec::new();
        for entry in std::fs::read_dir(&images_dir).map_err(Self::io_err)? {
            let entry = entry.map_err(Self::io_err)?;
            let meta_path = entry.path().join("meta.json");
            if !meta_path.exists() {
                continue;
            }
            let bytes = std::fs::read(&meta_path).map_err(Self::io_err)?;
            if let Ok(stored) = serde_json::from_slice::<StoredImageMeta>(&bytes) {
                result.push(ImageMeta::from(stored));
            }
        }
        Ok(result)
    }

    async fn find_blob_candidates(
        &self,
        base_image_id: &str,
        _partition_number: Option<i32>,
    ) -> Result<Vec<BlobCandidate>> {
        let inner = self.inner.lock().unwrap();
        let candidates = inner
            .blob_origins
            .iter()
            .filter(|r| r.orig_image_id == base_image_id)
            .filter_map(|r| {
                r.blob_uuid.parse::<Uuid>().ok().map(|uuid| {
                    // Look up sha256 from the reverse index.
                    let sha256 = inner
                        .sha256_index
                        .iter()
                        .find_map(|(k, e)| {
                            if e.uuid == uuid {
                                Some(k.clone())
                            } else {
                                None
                            }
                        })
                        .unwrap_or_default();
                    BlobCandidate {
                        uuid,
                        sha256,
                        original_path: r.file_path.clone(),
                    }
                })
            })
            .collect();
        Ok(candidates)
    }

    async fn record_blob_origin(
        &self,
        blob_uuid: Uuid,
        orig_image_id: &str,
        base_image_id: Option<&str>,
        _partition_number: Option<i32>,
        file_path: &str,
    ) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        inner.blob_origins.push(BlobOriginRecord {
            blob_uuid: blob_uuid.to_string(),
            orig_image_id: orig_image_id.to_string(),
            base_image_id: base_image_id.map(|s| s.to_string()),
            file_path: file_path.to_string(),
        });
        Self::save_blob_origins(&self.base_dir, &inner.blob_origins).map_err(Self::io_err)
    }

    async fn delete_manifest(&self, image_id: &str) -> Result<()> {
        let path = self.manifest_path(image_id);
        if path.exists() {
            std::fs::remove_file(&path).map_err(Self::io_err)?;
        }
        Ok(())
    }

    async fn delete_patches(&self, image_id: &str) -> Result<()> {
        let path = self.patches_path(image_id);
        if path.exists() {
            std::fs::remove_file(&path).map_err(Self::io_err)?;
        }
        Ok(())
    }

    async fn delete_blob(&self, blob_id: Uuid) -> Result<()> {
        let path = self.blob_path(blob_id);
        if path.exists() {
            std::fs::remove_file(&path).map_err(Self::io_err)?;
        }
        let mut inner = self.inner.lock().unwrap();
        inner.sha256_index.retain(|_, e| e.uuid != blob_id);
        inner.compressed_blobs.remove(&blob_id);
        Self::save_sha256_index(&self.base_dir, &inner.sha256_index).map_err(Self::io_err)?;
        Ok(())
    }

    async fn delete_blob_origins(&self, image_id: &str) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        inner.blob_origins.retain(|r| r.orig_image_id != image_id);
        Self::save_blob_origins(&self.base_dir, &inner.blob_origins).map_err(Self::io_err)
    }

    async fn delete_image_meta(&self, image_id: &str) -> Result<()> {
        let dir = self.image_dir(image_id);
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(Self::io_err)?;
        }
        Ok(())
    }
}
