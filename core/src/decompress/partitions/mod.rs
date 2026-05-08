// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// decompress/partitions — partition decompressor trait and per-type implementations

pub mod fs;

pub use fs::FsPartitionDecompressor;

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use crate::decompress::PartitionDecompressStats;
use crate::manifest::PartitionManifest;
use crate::routing::RouterEncoder;
use crate::storage::Storage;
use crate::Result;

// ── PartitionDecompressor trait ───────────────────────────────────────────────

/// Handles decompression for a single partition, regardless of type.
///
/// Implementations exist for:
/// - [`FsPartitionDecompressor`] — runs the 3-stage decompress pipeline.
#[async_trait]
#[allow(clippy::too_many_arguments)]
pub trait PartitionDecompressor: Send + Sync {
    /// Decompress one partition manifest into `output_root`.
    ///
    /// `base_root` is the mounted/extracted root of the previous version's
    /// matching partition (empty directory for full images).
    async fn decompress(
        &self,
        pm: &PartitionManifest,
        base_root: &Path,
        output_root: &Path,
        storage: Arc<dyn Storage>,
        archive_bytes: &[u8],
        patches_compressed: bool,
        router: Arc<RouterEncoder>,
        workers: usize,
    ) -> Result<PartitionDecompressStats>;
}
