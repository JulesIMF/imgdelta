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

use std::collections::HashMap;

use async_trait::async_trait;

use crate::compress::partitions::fs::context::StageContext;
use crate::manifest::PartitionManifest;
use crate::partitions::{MountHandle, PartitionHandle};
use crate::Result;

// ── PartitionCompressor trait ─────────────────────────────────────────────────

/// Handles compression for a single partition, regardless of type.
///
/// Implementations exist for:
/// - [`FsPartitionCompressor`] — mounts and runs the 8-stage pipeline.
/// - [`BiosBootCompressor`] — reads raw bytes and uploads as a single blob.
/// - [`RawPartitionCompressor`] — reads raw bytes and uploads as a single blob.
/// - [`MbrCompressor`] — reads 440 MBR bytes and uploads as a single blob.
#[async_trait]
pub trait PartitionCompressor: Send + Sync {
    /// Compress one partition and return a ready [`PartitionManifest`].
    ///
    /// Also returns `(patches_compressed, archive_stored_bytes)` so the caller
    /// can accumulate totals.
    async fn compress(
        &self,
        ctx: &StageContext,
        handle: PartitionHandle,
        fs_type: &str,
        base_partitions: &HashMap<u32, PartitionHandle>,
        live_mounts: &mut Vec<Box<dyn MountHandle>>,
        live_tmpdirs: &mut Vec<tempfile::TempDir>,
    ) -> Result<(PartitionManifest, bool, u64)>;
}
