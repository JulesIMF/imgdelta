// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// compress/partitions — partition compressor trait and per-type implementations

pub mod bios_boot;
pub mod fs;
pub mod mbr;
pub mod raw_partition;

pub use bios_boot::BiosBootCompressor;
pub use fs::FsPartitionCompressor;
pub use mbr::MbrCompressor;
pub use raw_partition::RawPartitionCompressor;

use async_trait::async_trait;

use crate::compress::context::CompressContext;
use crate::manifest::PartitionManifest;
use crate::partitions::PartitionHandle;
use crate::Result;

// ── PartitionCompressor trait ─────────────────────────────────────────────────

/// Handles compression for a single partition, regardless of type.
///
/// Implementations exist for:
/// - [`FsPartitionCompressor`] — mounts and runs the 8-stage pipeline, writing
///   patch files into `ctx.patches_dir`.
/// - [`BiosBootCompressor`] — reads raw bytes and uploads as a single blob.
/// - [`RawPartitionCompressor`] — reads raw bytes and uploads as a single blob.
/// - [`MbrCompressor`] — reads 440 MBR bytes and uploads as a single blob.
///
/// Patch bytes are never returned in memory.  FS compressors write individual
/// patch files to `ctx.patches_dir`; the orchestrator packs them into one
/// archive after all partitions are done.
#[async_trait]
pub trait PartitionCompressor: Send + Sync {
    /// Compress one partition and return its manifest.
    /// The second element is the number of bytes actually written to blob storage
    /// (deduped blobs already in storage are not counted).
    /// For Fs partitions this is always 0 — their blobs are tracked inside
    /// [`PartitionContent::Fs::blobs_stored_bytes`] and summed by the orchestrator.
    async fn compress(
        &self,
        ctx: &CompressContext,
        handle: PartitionHandle,
    ) -> Result<(PartitionManifest, u64)>;
}
