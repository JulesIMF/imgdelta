use std::path::{Path, PathBuf};

use async_trait::async_trait;

use crate::{Result, Storage};

// ── Stats ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct CompressionStats {
    pub files_patched: usize,
    pub files_added: usize,
    pub files_removed: usize,
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
    #[allow(dead_code)]
    storage: std::sync::Arc<dyn Storage>,
    #[allow(dead_code)]
    router: std::sync::Arc<crate::routing::RouterEncoder>,
}

impl DefaultCompressor {
    pub fn new(
        storage: std::sync::Arc<dyn Storage>,
        router: std::sync::Arc<crate::routing::RouterEncoder>,
    ) -> Self {
        Self { storage, router }
    }

    pub fn with_encoder(
        storage: std::sync::Arc<dyn Storage>,
        encoder: std::sync::Arc<dyn crate::encoder::PatchEncoder>,
    ) -> Self {
        Self::new(
            storage,
            std::sync::Arc::new(crate::routing::RouterEncoder::new(vec![], encoder)),
        )
    }
}

#[async_trait]
impl Compressor for DefaultCompressor {
    async fn compress(
        &self,
        _source_root: &Path,
        _target_root: &Path,
        _options: CompressOptions,
    ) -> Result<CompressionStats> {
        unimplemented!("compress is being redesigned in Phase 6.B")
    }

    async fn decompress(
        &self,
        _output_root: &Path,
        _options: DecompressOptions,
    ) -> Result<DecompressionStats> {
        unimplemented!("decompress is being redesigned in Phase 6.E")
    }
}
