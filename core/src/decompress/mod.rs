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

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use tracing::info;

use crate::manifest::Record;
use crate::routing::RouterEncoder;
use crate::storage::Storage;
use crate::Result;

use stages::{apply_records_fn, copy_unchanged_fn, extract_archive_fn};

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
    router: &RouterEncoder,
    workers: usize,
) -> Result<PartitionDecompressStats> {
    // Stage 1: extract patch archive.
    info!(
        records = records.len(),
        archive_bytes = archive_bytes.len(),
        patches_compressed,
        "decompress: stage 1/3 extract archive"
    );
    let patch_map = if archive_bytes.is_empty() {
        std::collections::HashMap::new()
    } else {
        extract_archive_fn(archive_bytes, patches_compressed)?
    };
    info!(
        patches_in_archive = patch_map.len(),
        "decompress: stage 1/3 done"
    );

    // Stage 2: copy unchanged base files.
    let affected: HashSet<String> = records.iter().filter_map(|r| r.old_path.clone()).collect();
    info!(
        affected = affected.len(),
        "decompress: stage 2/3 copy unchanged"
    );
    let copy_stats = copy_unchanged_fn(base_root, output_root, &affected)?;
    info!(
        files_copied = copy_stats.files_written,
        "decompress: stage 2/3 done"
    );

    // Stage 3: apply manifest records.
    info!(
        records = records.len(),
        workers, "decompress: stage 3/3 apply records"
    );
    let record_stats = apply_records_fn(
        records,
        base_root,
        output_root,
        &patch_map,
        storage,
        router,
        workers,
    )
    .await?;

    let stats = PartitionDecompressStats {
        files_written: copy_stats.files_written + record_stats.files_written,
        patches_verified: record_stats.patches_verified,
        bytes_written: copy_stats.bytes_written + record_stats.bytes_written,
    };

    info!(
        files_written = stats.files_written,
        bytes_written = stats.bytes_written,
        patches_verified = stats.patches_verified,
        "decompress: all stages done"
    );

    Ok(stats)
}
