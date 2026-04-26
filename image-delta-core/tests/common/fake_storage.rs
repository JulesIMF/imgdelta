#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use uuid::Uuid;

use image_delta_core::storage::{BlobCandidate, ImageMeta, ImageStatus};
use image_delta_core::{Result, Storage};

/// In-memory [`Storage`] implementation for L1 integration tests.
///
/// Thread-safe via an inner `Mutex`.  Suitable for use with
/// [`DefaultCompressor`] in multi-threaded test scenarios.
///
/// [`DefaultCompressor`]: image_delta_core::DefaultCompressor
#[derive(Debug, Default)]
struct FakeStorageInner {
    /// uuid → bytes
    blobs: HashMap<Uuid, Vec<u8>>,
    /// sha256 hex → uuid (dedup index)
    sha256_index: HashMap<String, Uuid>,
    manifests: HashMap<String, Vec<u8>>,
    patches: HashMap<String, Vec<u8>>,
    images: HashMap<String, ImageMeta>,
    /// image_id → list of (blob_uuid, file_path) for BlobPatch lookup.
    blob_origins: HashMap<String, Vec<(Uuid, String)>>,
}

#[derive(Debug, Default, Clone)]
pub struct FakeStorage {
    inner: Arc<Mutex<FakeStorageInner>>,
}

impl FakeStorage {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a blob that came from `image_id` at `file_path`.
    ///
    /// Used to set up `find_blob_candidates` in BlobPatch tests.
    pub fn register_blob_origin(&self, image_id: &str, blob_id: Uuid, file_path: &str) {
        self.inner
            .lock()
            .unwrap()
            .blob_origins
            .entry(image_id.to_string())
            .or_default()
            .push((blob_id, file_path.to_string()));
    }
}

impl Storage for FakeStorage {
    fn blob_exists(&self, sha256: &str) -> Result<Option<Uuid>> {
        Ok(self.inner.lock().unwrap().sha256_index.get(sha256).copied())
    }

    fn upload_blob(&self, sha256: &str, data: &[u8]) -> Result<Uuid> {
        let mut inner = self.inner.lock().unwrap();
        if let Some(&existing) = inner.sha256_index.get(sha256) {
            return Ok(existing);
        }
        let id = Uuid::new_v4();
        inner.blobs.insert(id, data.to_vec());
        inner.sha256_index.insert(sha256.to_string(), id);
        Ok(id)
    }

    fn download_blob(&self, blob_id: Uuid) -> Result<Vec<u8>> {
        self.inner
            .lock()
            .unwrap()
            .blobs
            .get(&blob_id)
            .cloned()
            .ok_or_else(|| image_delta_core::Error::Storage(format!("blob not found: {blob_id}")))
    }

    fn upload_manifest(&self, image_id: &str, manifest_bytes: &[u8]) -> Result<()> {
        self.inner
            .lock()
            .unwrap()
            .manifests
            .insert(image_id.to_string(), manifest_bytes.to_vec());
        Ok(())
    }

    fn download_manifest(&self, image_id: &str) -> Result<Vec<u8>> {
        self.inner
            .lock()
            .unwrap()
            .manifests
            .get(image_id)
            .cloned()
            .ok_or_else(|| {
                image_delta_core::Error::Storage(format!("manifest not found: {image_id}"))
            })
    }

    fn upload_patches(&self, image_id: &str, data: &[u8], _compressed: bool) -> Result<()> {
        self.inner
            .lock()
            .unwrap()
            .patches
            .insert(image_id.to_string(), data.to_vec());
        Ok(())
    }

    fn download_patches(&self, image_id: &str) -> Result<Vec<u8>> {
        self.inner
            .lock()
            .unwrap()
            .patches
            .get(image_id)
            .cloned()
            .ok_or_else(|| {
                image_delta_core::Error::Storage(format!("patches not found: {image_id}"))
            })
    }

    fn register_image(&self, meta: &ImageMeta) -> Result<()> {
        self.inner
            .lock()
            .unwrap()
            .images
            .insert(meta.image_id.clone(), meta.clone());
        Ok(())
    }

    fn get_image(&self, image_id: &str) -> Result<Option<ImageMeta>> {
        Ok(self.inner.lock().unwrap().images.get(image_id).cloned())
    }

    fn update_status(&self, _image_id: &str, _status: ImageStatus) -> Result<()> {
        Ok(())
    }

    fn list_images(&self) -> Result<Vec<ImageMeta>> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .images
            .values()
            .cloned()
            .collect())
    }

    fn find_blob_candidates(&self, base_image_id: &str) -> Result<Vec<BlobCandidate>> {
        let inner = self.inner.lock().unwrap();
        let origins = match inner.blob_origins.get(base_image_id) {
            Some(o) => o,
            None => return Ok(Vec::new()),
        };
        let candidates = origins
            .iter()
            .filter_map(|(blob_id, path)| {
                // Look up sha256 for this UUID via the reverse index.
                let sha256 = inner
                    .sha256_index
                    .iter()
                    .find_map(|(k, &v)| if v == *blob_id { Some(k.clone()) } else { None })
                    .unwrap_or_default();
                inner.blobs.get(blob_id).map(|_| BlobCandidate {
                    uuid: *blob_id,
                    sha256,
                    original_path: path.clone(),
                })
            })
            .collect();
        Ok(candidates)
    }

    fn record_blob_origin(&self, blob_uuid: Uuid, image_id: &str, file_path: &str) -> Result<()> {
        self.inner
            .lock()
            .unwrap()
            .blob_origins
            .entry(image_id.to_string())
            .or_default()
            .push((blob_uuid, file_path.to_string()));
        Ok(())
    }
}
