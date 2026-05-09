// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// operations — top-level orchestrator functions, options, and stats types

use std::path::PathBuf;

pub mod compress;
pub mod decompress;
pub mod delete;

pub use compress::compress;
pub use decompress::decompress;
pub use delete::delete_image;

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
    pub debug_dir: Option<PathBuf>,
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
