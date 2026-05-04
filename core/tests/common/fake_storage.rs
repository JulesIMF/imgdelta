// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// FakeStorage: in-memory Storage implementation for integration tests

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
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
    /// image_id → (tar bytes, compressed flag)
    patches: HashMap<String, (Vec<u8>, bool)>,
    images: HashMap<String, ImageMeta>,
    /// image_id → list of (blob_uuid, file_path) for BlobPatch lookup.
    blob_origins: HashMap<String, Vec<(Uuid, String)>>,
    /// Total number of times `upload_blob` was called (including dedup hits).
    upload_call_count: usize,
    /// Total number of times `blob_exists` was called.
    blob_exists_call_count: usize,
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

    /// Returns `true` if a patches archive was uploaded for `image_id`.
    pub fn has_patches(&self, image_id: &str) -> bool {
        self.inner.lock().unwrap().patches.contains_key(image_id)
    }

    /// Returns the number of distinct blobs currently stored.
    pub fn uploaded_blob_count(&self) -> usize {
        self.inner.lock().unwrap().blobs.len()
    }

    /// Returns the total number of times `upload_blob` was called,
    /// including calls for content that was already stored (dedup hits).
    pub fn upload_call_count(&self) -> usize {
        self.inner.lock().unwrap().upload_call_count
    }

    /// Returns the total number of times `blob_exists` was called.
    pub fn blob_exists_call_count(&self) -> usize {
        self.inner.lock().unwrap().blob_exists_call_count
    }

    /// Returns `Some(compressed)` for the most recent `upload_patches` call
    /// for `image_id`, or `None` if no patches were uploaded for that id.
    pub fn patches_were_compressed(&self, image_id: &str) -> Option<bool> {
        self.inner
            .lock()
            .unwrap()
            .patches
            .get(image_id)
            .map(|(_, c)| *c)
    }

    /// Returns the current status string for `image_id`, or `None` if the
    /// image has never been registered.
    pub fn image_status(&self, image_id: &str) -> Option<String> {
        self.inner
            .lock()
            .unwrap()
            .images
            .get(image_id)
            .map(|m| m.status.clone())
    }

    /// Returns the raw manifest bytes stored for `image_id`, or `None`.
    pub fn get_manifest(&self, image_id: &str) -> Option<Vec<u8>> {
        self.inner.lock().unwrap().manifests.get(image_id).cloned()
    }
}

#[async_trait]
impl Storage for FakeStorage {
    async fn blob_exists(&self, sha256: &str) -> Result<Option<Uuid>> {
        let mut inner = self.inner.lock().unwrap();
        inner.blob_exists_call_count += 1;
        Ok(inner.sha256_index.get(sha256).copied())
    }

    async fn upload_blob(&self, sha256: &str, data: &[u8]) -> Result<Uuid> {
        let mut inner = self.inner.lock().unwrap();
        inner.upload_call_count += 1;
        if let Some(&existing) = inner.sha256_index.get(sha256) {
            return Ok(existing);
        }
        let id = Uuid::new_v4();
        inner.blobs.insert(id, data.to_vec());
        inner.sha256_index.insert(sha256.to_string(), id);
        Ok(id)
    }

    async fn download_blob(&self, blob_id: Uuid) -> Result<Vec<u8>> {
        self.inner
            .lock()
            .unwrap()
            .blobs
            .get(&blob_id)
            .cloned()
            .ok_or_else(|| image_delta_core::Error::Storage(format!("blob not found: {blob_id}")))
    }

    async fn upload_manifest(&self, image_id: &str, manifest_bytes: &[u8]) -> Result<()> {
        self.inner
            .lock()
            .unwrap()
            .manifests
            .insert(image_id.to_string(), manifest_bytes.to_vec());
        Ok(())
    }

    async fn download_manifest(&self, image_id: &str) -> Result<Vec<u8>> {
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

    async fn upload_patches(&self, image_id: &str, data: &[u8], compressed: bool) -> Result<()> {
        self.inner
            .lock()
            .unwrap()
            .patches
            .insert(image_id.to_string(), (data.to_vec(), compressed));
        Ok(())
    }

    async fn download_patches(&self, image_id: &str) -> Result<Vec<u8>> {
        self.inner
            .lock()
            .unwrap()
            .patches
            .get(image_id)
            .map(|(data, _)| data.clone())
            .ok_or_else(|| {
                image_delta_core::Error::Storage(format!("patches not found: {image_id}"))
            })
    }

    async fn register_image(&self, meta: &ImageMeta) -> Result<()> {
        self.inner
            .lock()
            .unwrap()
            .images
            .insert(meta.image_id.clone(), meta.clone());
        Ok(())
    }

    async fn get_image(&self, image_id: &str) -> Result<Option<ImageMeta>> {
        Ok(self.inner.lock().unwrap().images.get(image_id).cloned())
    }

    async fn update_status(&self, image_id: &str, status: ImageStatus) -> Result<()> {
        let status_str = match &status {
            ImageStatus::Pending => "pending".to_string(),
            ImageStatus::Compressing => "compressing".to_string(),
            ImageStatus::Compressed => "compressed".to_string(),
            ImageStatus::Failed(msg) => format!("failed: {msg}"),
        };
        let mut inner = self.inner.lock().unwrap();
        if let Some(meta) = inner.images.get_mut(image_id) {
            meta.status = status_str;
        }
        Ok(())
    }

    async fn list_images(&self) -> Result<Vec<ImageMeta>> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .images
            .values()
            .cloned()
            .collect())
    }

    async fn find_blob_candidates(&self, base_image_id: &str) -> Result<Vec<BlobCandidate>> {
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

    async fn record_blob_origin(
        &self,
        blob_uuid: Uuid,
        image_id: &str,
        _base_image_id: Option<&str>,
        file_path: &str,
    ) -> Result<()> {
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
