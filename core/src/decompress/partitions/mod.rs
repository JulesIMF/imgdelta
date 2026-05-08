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

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use crate::decompress::PartitionDecompressStats;
use crate::manifest::PartitionManifest;
use crate::partitions::PartitionHandle;
use crate::routing::RouterEncoder;
use crate::storage::Storage;
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
#[allow(clippy::too_many_arguments)]
pub trait PartitionDecompressor: Send + Sync {
    /// Decompress one partition manifest into `output_ph`.
    ///
    /// `base_root` is the mounted root of the previous version's matching
    /// partition (empty directory for full images, ignored by binary types).
    ///
    /// `output_ph` must be a **writable** [`PartitionHandle`]:
    /// - `PartitionHandle::Fs`       — its `mount_fn` must return an RW mount.
    /// - `PartitionHandle::BiosBoot` — must have a `write_fn` set.
    /// - `PartitionHandle::Raw`      — must have a `write_fn` set.
    /// - `PartitionHandle::Mbr`      — must have a `write_fn` set.
    async fn decompress(
        &self,
        pm: &PartitionManifest,
        base_root: &Path,
        output_ph: &PartitionHandle,
        storage: Arc<dyn Storage>,
        archive_bytes: &[u8],
        patches_compressed: bool,
        router: Arc<RouterEncoder>,
        workers: usize,
    ) -> Result<PartitionDecompressStats>;
}
