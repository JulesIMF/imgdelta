// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Decompress pipeline: module root and public entry point

//! Three-stage stateless decompress pipeline for one `Fs` partition.
//!
//! Stages:
//! 1. [`stages::ExtractArchive`] — decompress/index the patches tar archive
//! 2. [`stages::CopyUnchanged`] — copy unchanged base files to output
//! 3. [`stages::ApplyRecords`] — download blobs + apply all manifest records
//!
//! The public entry point [`decompress_fs_partition`] has the same signature as
//! the old `decompress_pipeline::decompress_fs_partition` and is a drop-in
//! replacement.

pub mod context;
pub mod draft;
pub mod pipeline;
pub mod stage;
pub mod stages;

use std::path::Path;
use std::sync::Arc;

use crate::manifest::Record;
use crate::routing::RouterEncoder;
use crate::storage::Storage;
use crate::Result;

use context::DecompressContext;
use draft::DecompressDraft;
use pipeline::DecompressPipeline;

// ── Public stats type ─────────────────────────────────────────────────────────

/// Per-partition decompress statistics.
#[derive(Debug, Default)]
pub struct PartitionDecompressStats {
    pub files_written: usize,
    pub patches_verified: usize,
    pub bytes_written: u64,
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Reconstruct an Fs partition into `output_root` from `base_root` + manifest records.
///
/// Drop-in replacement for `decompress_pipeline::decompress_fs_partition`.
#[allow(clippy::too_many_arguments)]
pub async fn decompress_fs_partition(
    base_root: &Path,
    output_root: &Path,
    records: &[Record],
    archive_bytes: &[u8],
    patches_compressed: bool,
    storage: Arc<dyn Storage>,
    router: Arc<RouterEncoder>,
    workers: usize,
) -> Result<PartitionDecompressStats> {
    let ctx = DecompressContext {
        storage,
        router,
        workers,
        base_root: Arc::from(base_root),
        output_root: Arc::from(output_root),
        records: Arc::from(records),
        archive_bytes: Arc::from(archive_bytes),
        patches_compressed,
    };

    let pipeline = DecompressPipeline::default_fs();
    let draft = pipeline.run(&ctx, DecompressDraft::default()).await?;

    Ok(draft.stats)
}
