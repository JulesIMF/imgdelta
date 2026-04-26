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
    blobs: HashMap<Uuid, Vec<u8>>,
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
    fn upload_blob(&self, data: &[u8]) -> Result<Uuid> {
        let id = Uuid::new_v4();
        self.inner.lock().unwrap().blobs.insert(id, data.to_vec());
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

    fn find_blob_candidates(&self, base_image_id: &str) -> Result<Vec<BlobCandidate>> {
        let inner = self.inner.lock().unwrap();
        let origins = match inner.blob_origins.get(base_image_id) {
            Some(o) => o,
            None => return Ok(Vec::new()),
        };
        let candidates = origins
            .iter()
            .filter_map(|(blob_id, path)| {
                inner.blobs.get(blob_id).map(|data| BlobCandidate {
                    blob_id: *blob_id,
                    path: path.clone(),
                    size: data.len() as u64,
                })
            })
            .collect();
        Ok(candidates)
    }

    fn save_image_meta(&self, meta: &ImageMeta) -> Result<()> {
        self.inner
            .lock()
            .unwrap()
            .images
            .insert(meta.image_id.clone(), meta.clone());
        Ok(())
    }

    fn get_image_meta(&self, image_id: &str) -> Result<Option<ImageMeta>> {
        Ok(self.inner.lock().unwrap().images.get(image_id).cloned())
    }

    fn set_image_status(&self, _image_id: &str, _status: ImageStatus) -> Result<()> {
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

    fn upload_patches(&self, image_id: &str, data: &[u8]) -> Result<()> {
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
}
