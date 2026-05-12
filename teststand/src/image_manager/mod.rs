// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Jules IMF
//
// image-delta — incremental disk-image compression toolkit
// ImageManager: download and cache disk images from S3.

//! Downloads, caches, and evicts image files.
//!
//! State machine per image ID:
//!   Missing → Downloading → Ready → Evicting → Missing

use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tokio::sync::{Mutex, Notify};
use tracing::{error, info};

use crate::{
    config::families::ImageSpec,
    error::{Error, Result},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageState {
    Missing,
    Downloading {
        progress_bytes: u64,
        total_bytes: Option<u64>,
    },
    Ready,
    Evicting,
}

struct ImageEntry {
    spec: ImageSpec,
    state: ImageState,
    /// Absolute path on disk (valid when state == Ready).
    path: Option<PathBuf>,
}

pub struct ImageManager {
    images_dir: PathBuf,
    state: Mutex<HashMap<String, ImageEntry>>,
    notify: Notify,
    client: reqwest::Client,
}

impl ImageManager {
    pub fn new(images_dir: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            images_dir,
            state: Mutex::new(HashMap::new()),
            notify: Notify::new(),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(3600))
                .build()
                .expect("reqwest client"),
        })
    }

    /// Register a set of image specs.  Already-known images are not reset.
    pub async fn register(&self, specs: impl IntoIterator<Item = ImageSpec>) {
        let mut map = self.state.lock().await;
        for spec in specs {
            map.entry(spec.id.clone()).or_insert_with(|| ImageEntry {
                spec,
                state: ImageState::Missing,
                path: None,
            });
        }
    }

    /// Ensure the image is downloaded.  Waits if already downloading.
    /// Returns the local path when ready.
    pub async fn ensure(&self, id: &str) -> Result<PathBuf> {
        loop {
            let needs_download = {
                let map = self.state.lock().await;
                match map.get(id) {
                    None => return Err(Error::NotFound(format!("image {id} not registered"))),
                    Some(e) => match &e.state {
                        ImageState::Ready => return Ok(e.path.clone().unwrap()),
                        ImageState::Downloading { .. } => false,
                        ImageState::Missing => true,
                        ImageState::Evicting => false,
                    },
                }
            };
            if needs_download {
                self.start_download(id).await?;
            } else {
                // Wait for state change from Downloading → Ready/Missing
                self.notify.notified().await;
            }
        }
    }

    async fn start_download(&self, id: &str) -> Result<()> {
        let (url, sha256, _size_bytes) = {
            let mut map = self.state.lock().await;
            let entry = map
                .get_mut(id)
                .ok_or_else(|| Error::NotFound(id.to_owned()))?;
            if entry.state != ImageState::Missing {
                return Ok(()); // already being handled
            }
            entry.state = ImageState::Downloading {
                progress_bytes: 0,
                total_bytes: None,
            };
            (
                entry.spec.url.clone(),
                entry.spec.sha256.clone(),
                entry.spec.size_bytes,
            )
        };
        self.notify.notify_waiters();

        let images_dir = self.images_dir.clone();
        let id_owned = id.to_owned();
        let client = self.client.clone();
        let notify = &self.notify;

        // Download in current task (caller holds no locks)
        let result = download_image(&client, &url, &images_dir, &id_owned, sha256.as_deref()).await;

        match result {
            Ok(path) => {
                let mut map = self.state.lock().await;
                if let Some(e) = map.get_mut(&id_owned) {
                    e.state = ImageState::Ready;
                    e.path = Some(path);
                }
                info!(id = %id_owned, "image ready");
                notify.notify_waiters();
            }
            Err(err) => {
                error!(id = %id_owned, err = %err, "download failed");
                let mut map = self.state.lock().await;
                if let Some(e) = map.get_mut(&id_owned) {
                    e.state = ImageState::Missing;
                }
                notify.notify_waiters();
            }
        }
        Ok(())
    }

    /// Evict (delete) an image to free disk space.
    pub async fn evict(&self, id: &str) -> Result<()> {
        let path = {
            let mut map = self.state.lock().await;
            let entry = map
                .get_mut(id)
                .ok_or_else(|| Error::NotFound(id.to_owned()))?;
            if entry.state != ImageState::Ready {
                return Ok(());
            }
            entry.state = ImageState::Evicting;
            entry.path.clone()
        };
        if let Some(p) = path {
            if p.exists() {
                if p.is_dir() {
                    tokio::fs::remove_dir_all(&p).await?;
                } else {
                    tokio::fs::remove_file(&p).await?;
                }
            }
        }
        let mut map = self.state.lock().await;
        if let Some(e) = map.get_mut(id) {
            e.state = ImageState::Missing;
            e.path = None;
        }
        self.notify.notify_waiters();
        Ok(())
    }

    pub async fn image_state(&self, id: &str) -> Option<String> {
        let map = self.state.lock().await;
        map.get(id).map(|e| match &e.state {
            ImageState::Missing => "missing".into(),
            ImageState::Downloading {
                progress_bytes,
                total_bytes,
            } => {
                format!(
                    "downloading:{}/{}",
                    progress_bytes,
                    total_bytes.map_or("?".to_string(), |n| n.to_string())
                )
            }
            ImageState::Ready => "ready".into(),
            ImageState::Evicting => "evicting".into(),
        })
    }

    /// List all registered images with their current state.
    /// Also lazily detects images that are already on disk (Missing → Ready).
    pub async fn list_all(&self) -> Vec<ImageInfo> {
        let mut map = self.state.lock().await;
        for entry in map.values_mut() {
            if entry.state == ImageState::Missing {
                let dest = self.images_dir.join(format!("{}.qcow2", entry.spec.id));
                if dest.exists() {
                    entry.state = ImageState::Ready;
                    entry.path = Some(dest);
                }
            }
        }
        map.values()
            .map(|e| {
                let (state, progress_bytes, total_bytes) = match &e.state {
                    ImageState::Missing => ("missing".to_owned(), None, None),
                    ImageState::Downloading {
                        progress_bytes,
                        total_bytes,
                    } => (
                        "downloading".to_owned(),
                        Some(*progress_bytes),
                        *total_bytes,
                    ),
                    ImageState::Ready => ("ready".to_owned(), None, None),
                    ImageState::Evicting => ("evicting".to_owned(), None, None),
                };
                ImageInfo {
                    id: e.spec.id.clone(),
                    size_bytes: e.spec.size_bytes,
                    state,
                    progress_bytes,
                    total_bytes: total_bytes.or(e.spec.size_bytes),
                }
            })
            .collect()
    }
}

pub struct ImageInfo {
    pub id: String,
    #[allow(dead_code)]
    pub size_bytes: Option<u64>,
    pub state: String,
    pub progress_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
}

async fn download_image(
    client: &reqwest::Client,
    url: &str,
    images_dir: &PathBuf,
    id: &str,
    expected_sha256: Option<&str>,
) -> Result<PathBuf> {
    tokio::fs::create_dir_all(images_dir).await?;
    let dest = images_dir.join(format!("{}.qcow2", id));
    if dest.exists() {
        info!(id, "image already on disk, skipping download");
        return Ok(dest);
    }
    let tmp = images_dir.join(format!("{}.tmp", id));

    info!(id, %url, "starting download");
    let resp = client.get(url).send().await?;
    if !resp.status().is_success() {
        return Err(Error::Other(format!(
            "HTTP {} downloading {}",
            resp.status(),
            url
        )));
    }

    let mut file = tokio::fs::File::create(&tmp).await?;
    let mut hasher = Sha256::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let bytes = chunk?;
        hasher.update(&bytes);
        use tokio::io::AsyncWriteExt;
        file.write_all(&bytes).await?;
    }
    let digest = format!("{:x}", hasher.finalize());

    if let Some(expected) = expected_sha256 {
        if !bool::from(subtle::ConstantTimeEq::ct_eq(
            digest.as_bytes(),
            expected.as_bytes(),
        )) {
            tokio::fs::remove_file(&tmp).await.ok();
            return Err(Error::Other(format!(
                "SHA-256 mismatch for {id}: got {digest} expected {expected}"
            )));
        }
    }

    tokio::fs::rename(&tmp, &dest).await?;
    info!(id, "download complete");
    Ok(dest)
}
