// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// decompress/partitions — partition decompressor trait and per-type implementations

pub mod bios_boot;
pub mod fs;
pub mod mbr;
pub mod raw_partition;

pub use bios_boot::BiosBootDecompressor;
pub use fs::FsPartitionDecompressor;
pub use mbr::MbrDecompressor;
pub use raw_partition::RawPartitionDecompressor;

use async_trait::async_trait;

use crate::decompress::context::DecompressContext;
use crate::decompress::PartitionDecompressStats;
use crate::manifest::PartitionManifest;
use crate::partitions::PartitionHandle;
use crate::Result;

// ── PartitionDecompressor trait ───────────────────────────────────────────────

/// Handles decompression for a single partition, regardless of type.
///
/// Implementations exist for:
/// - [`FsPartitionDecompressor`]        — runs the 3-stage file-system decompress pipeline.
/// - [`BiosBootDecompressor`]           — downloads blob and calls `write_raw()` on the handle.
/// - [`RawPartitionDecompressor`]       — downloads blob and calls `write_raw()` on the handle.
/// - [`MbrDecompressor`]                — downloads blob and calls `write_raw()` on the handle.
#[async_trait]
pub trait PartitionDecompressor: Send + Sync {
    /// Decompress one partition manifest into `output_ph`.
    ///
    /// All shared inputs (storage, router, workers, patch map, base FS handles)
    /// are contained in `ctx`.  `pm` is the partition manifest to decompress;
    /// `output_ph` must be a **writable** [`PartitionHandle`]:
    /// - `PartitionHandle::Fs`       — its `mount_fn` must return an RW mount.
    /// - `PartitionHandle::BiosBoot` — must have a `write_fn` set.
    /// - `PartitionHandle::Raw`      — must have a `write_fn` set.
    /// - `PartitionHandle::Mbr`      — must have a `write_fn` set.
    async fn decompress(
        &self,
        ctx: &DecompressContext,
        pm: &PartitionManifest,
        output_ph: &PartitionHandle,
    ) -> Result<PartitionDecompressStats>;
}
