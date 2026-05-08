// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Decompress pipeline: module root and public entry point

//! Three-stage stateless decompress pipeline for one `Fs` partition.
//!
//! Stages:
//! 1. [`partitions::fs::stages::ExtractArchive`] — decompress/index the patches tar archive
//! 2. [`partitions::fs::stages::CopyUnchanged`] — copy unchanged base files to output
//! 3. [`partitions::fs::stages::ApplyRecords`] — download blobs + apply all manifest records
//!
//! The public entry point [`decompress_fs_partition`] is a re-export from
//! [`partitions::fs::decompress_fs_partition`].

pub mod partitions;

pub use partitions::fs::decompress_fs_partition;
pub use partitions::{FsPartitionDecompressor, PartitionDecompressor};

// ── Public stats type ─────────────────────────────────────────────────────────

/// Per-partition decompress statistics.
#[derive(Debug, Default)]
pub struct PartitionDecompressStats {
    pub files_written: usize,
    pub patches_verified: usize,
    pub bytes_written: u64,
}
