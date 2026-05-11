// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Jules IMF
//
// image-delta — incremental disk-image compression toolkit
// Calls image-delta-core compress/decompress directly.

use std::{path::Path, sync::Arc};
use tracing::{info, warn};

use image_delta_core::{
    operations::{compress, decompress},
    CompressOptions, CompressionStats, DecompressOptions, DecompressionStats, DirectoryImage,
    LocalStorage, Qcow2Image, RouterEncoder,
};

use crate::error::{Error, Result};

#[allow(dead_code)]
pub struct PairResult {
    pub image_id: String,
    pub base_image_id: Option<String>,
    pub compress_stats: CompressionStats,
    pub decompress_stats: Option<DecompressionStats>,
    pub archive_bytes: u64,
    pub base_file_bytes: u64,
    pub target_file_bytes: u64,
}

/// Build a minimal passthrough router (no delta encoding configured yet).
fn build_passthrough_router() -> Arc<RouterEncoder> {
    use image_delta_core::encoding::{PassthroughEncoder, RouterEncoder};
    Arc::new(RouterEncoder::new(
        vec![],
        Arc::new(PassthroughEncoder::new()),
    ))
}

/// Build an `Arc<dyn Image>` appropriate for the given format string.
/// Supported: "qcow2" (mounts via qemu-nbd), anything else → DirectoryImage.
fn make_image(format: &str) -> Arc<dyn image_delta_core::Image> {
    match format {
        "qcow2" => Arc::new(Qcow2Image::new()),
        _ => Arc::new(DirectoryImage::new()),
    }
}

/// Compress one (base, target) pair.
///
/// `target_path` / `base_path` are either:
///   - a `.qcow2` file (format = "qcow2")  → opened via qemu-nbd
///   - a directory    (format = "directory") → walked directly
#[allow(clippy::too_many_arguments)]
pub async fn compress_pair(
    image_id: &str,
    base_image_id: Option<&str>,
    target_path: &Path,
    base_path: &Path,
    storage_dir: &Path,
    workers: usize,
    passthrough_threshold: f64,
    run_decompress: bool,
    format: &str,
) -> Result<PairResult> {
    info!(
        image_id,
        ?base_image_id,
        workers,
        format,
        "compressing pair"
    );

    let storage: Arc<dyn image_delta_core::Storage> = Arc::new(
        LocalStorage::new(storage_dir.to_path_buf())
            .map_err(|e| Error::Other(format!("storage: {e}")))?,
    );
    let router = build_passthrough_router();

    let compress_stats = compress(
        make_image(format),
        Arc::clone(&storage),
        Arc::clone(&router),
        base_path,
        target_path,
        CompressOptions {
            image_id: image_id.to_owned(),
            base_image_id: base_image_id.map(str::to_owned),
            workers,
            passthrough_threshold,
            overwrite: true,
            debug_dir: None,
        },
    )
    .await
    .map_err(Error::Core)?;

    let archive_bytes = compress_stats.total_stored_bytes;

    let decompress_stats = if run_decompress {
        let decomp_dir = tempfile::tempdir().map_err(Error::Io)?;
        let storage2: Arc<dyn image_delta_core::Storage> = Arc::new(
            LocalStorage::new(storage_dir.to_path_buf())
                .map_err(|e| Error::Other(format!("storage2: {e}")))?,
        );
        let opts = DecompressOptions {
            image_id: image_id.to_owned(),
            base_root: base_path.to_path_buf(),
            workers,
        };
        match decompress(
            make_image(format),
            storage2,
            build_passthrough_router(),
            decomp_dir.path(),
            opts,
        )
        .await
        {
            Ok(s) => Some(s),
            Err(e) => {
                warn!(image_id, err = %e, "decompress round-trip failed");
                None
            }
        }
    } else {
        None
    };

    let base_file_bytes = file_or_dir_size(base_path)?;
    let target_file_bytes = file_or_dir_size(target_path)?;

    Ok(PairResult {
        image_id: image_id.to_owned(),
        base_image_id: base_image_id.map(str::to_owned),
        compress_stats,
        decompress_stats,
        archive_bytes,
        base_file_bytes,
        target_file_bytes,
    })
}

/// Sum bytes of all files under a directory (or return file size for a single file).
pub fn dir_size_bytes(path: &Path) -> Result<u64> {
    file_or_dir_size(path)
}

/// Sum bytes of a path: single file → its size; directory → recursive sum.
fn file_or_dir_size(path: &Path) -> Result<u64> {
    if path.is_file() {
        return Ok(path.metadata().map(|m| m.len()).unwrap_or(0));
    }
    let mut total = 0u64;
    for entry in walkdir::WalkDir::new(path).follow_links(false) {
        let entry = entry.map_err(|e| Error::Other(e.to_string()))?;
        if entry.file_type().is_file() {
            total += entry.metadata().map(|m| m.len()).unwrap_or(0);
        }
    }
    Ok(total)
}
