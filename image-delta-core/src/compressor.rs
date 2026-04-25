use crate::{DeltaEncoder, Result, Storage};
use std::sync::Arc;

/// Statistics produced by a compress operation.
#[derive(Debug, Clone, Default)]
pub struct CompressionStats {
    pub total_files: usize,
    /// Files stored as deltas against a base blob.
    pub delta_files: usize,
    /// Files stored verbatim (new files or passthrough-encoded).
    pub verbatim_files: usize,
    pub total_source_bytes: u64,
    pub total_stored_bytes: u64,
    pub elapsed_secs: f64,
}

impl CompressionStats {
    /// Compression ratio: stored / source.  Lower is better.
    pub fn ratio(&self) -> f64 {
        if self.total_source_bytes == 0 {
            return 1.0;
        }
        self.total_stored_bytes as f64 / self.total_source_bytes as f64
    }
}

/// Statistics produced by a decompress operation.
#[derive(Debug, Clone, Default)]
pub struct DecompressionStats {
    pub total_files: usize,
    pub total_bytes: u64,
    pub elapsed_secs: f64,
}

/// Options for a compress operation.
pub struct CompressOptions {
    /// Provider-assigned identifier for the image being compressed.
    pub image_id: String,
    /// Provider-assigned identifier for the base image.
    pub base_image_id: Option<String>,
    /// Number of parallel worker threads.
    pub workers: usize,
    /// If `delta_size >= source_size * threshold`, fall back to passthrough.
    /// Default: `1.0` (always prefer delta if it is any smaller).
    pub passthrough_threshold: f64,
}

/// Options for a decompress operation.
pub struct DecompressOptions {
    pub image_id: String,
    pub workers: usize,
}

/// High-level compress/decompress operations.
///
/// The default implementation [`DefaultCompressor`] orchestrates the full
/// pipeline: diff → path-match → encode → upload.
pub trait Compressor: Send + Sync {
    /// Compress `target_root` relative to `source_root` and upload to storage.
    fn compress(
        &self,
        source_root: &std::path::Path,
        target_root: &std::path::Path,
        options: CompressOptions,
    ) -> Result<CompressionStats>;

    /// Download patches from storage and reconstruct the image at `output_root`.
    fn decompress(
        &self,
        output_root: &std::path::Path,
        options: DecompressOptions,
    ) -> Result<DecompressionStats>;
}

/// Production [`Compressor`] implementation.
///
/// Owns references to a [`Storage`] backend and a [`DeltaEncoder`] (which may
/// be a [`RouterEncoder`] for per-file encoder selection).
///
/// [`RouterEncoder`]: crate::RouterEncoder
pub struct DefaultCompressor {
    storage: Arc<dyn Storage>,
    encoder: Arc<dyn DeltaEncoder>,
}

impl DefaultCompressor {
    pub fn new(storage: Arc<dyn Storage>, encoder: Arc<dyn DeltaEncoder>) -> Self {
        Self { storage, encoder }
    }
}

impl Compressor for DefaultCompressor {
    fn compress(
        &self,
        _source_root: &std::path::Path,
        _target_root: &std::path::Path,
        _options: CompressOptions,
    ) -> Result<CompressionStats> {
        todo!("Phase 4: diff_dirs → path_match → scheduler → encode → upload")
    }

    fn decompress(
        &self,
        _output_root: &std::path::Path,
        _options: DecompressOptions,
    ) -> Result<DecompressionStats> {
        todo!("Phase 4: download manifest → scheduler → decode → write")
    }
}
