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

/// Per-stage timing breakdown for one `compress()` call.
///
/// All fields are wall-clock milliseconds.  Multiple Fs partitions are
/// accumulated (summed) into a single `StageTimings` value.
/// Non-Fs partitions (MBR, BIOS boot, raw) do not contribute to stage
/// timings because they do not run the 8-stage pipeline.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct StageTimings {
    pub walkdir_ms: u64,
    pub blob_lookup_ms: u64,
    pub match_renamed_ms: u64,
    pub cleanup_ms: u64,
    pub upload_blobs_ms: u64,
    pub download_blobs_ms: u64,
    pub compute_patches_ms: u64,
    pub pack_archive_ms: u64,
}

impl std::ops::AddAssign for StageTimings {
    fn add_assign(&mut self, rhs: Self) {
        self.walkdir_ms += rhs.walkdir_ms;
        self.blob_lookup_ms += rhs.blob_lookup_ms;
        self.match_renamed_ms += rhs.match_renamed_ms;
        self.cleanup_ms += rhs.cleanup_ms;
        self.upload_blobs_ms += rhs.upload_blobs_ms;
        self.download_blobs_ms += rhs.download_blobs_ms;
        self.compute_patches_ms += rhs.compute_patches_ms;
        self.pack_archive_ms += rhs.pack_archive_ms;
    }
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CompressionStats {
    pub files_patched: usize,
    pub files_added: usize,
    pub files_removed: usize,
    pub files_renamed: usize,
    pub total_source_bytes: u64,
    pub total_stored_bytes: u64,
    pub elapsed_secs: f64,
    /// Per-stage timing breakdown.  `None` when the compress context did not
    /// attach a timing sink (e.g. legacy test helpers).
    pub stage_timings: Option<StageTimings>,
}

impl CompressionStats {
    pub fn ratio(&self) -> f64 {
        if self.total_source_bytes == 0 {
            return 1.0;
        }
        self.total_stored_bytes as f64 / self.total_source_bytes as f64
    }
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
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
