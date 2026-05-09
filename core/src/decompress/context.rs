// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// decompress/context — shared decompress context passed to all partition decompressors

use std::collections::HashMap;
use std::sync::Arc;

use crate::encoding::RouterEncoder;
use crate::storage::Storage;

// ── DecompressContext ─────────────────────────────────────────────────────────

/// Immutable inputs shared across all [`PartitionDecompressor`] impls for one
/// decompression run.
///
/// Mirrors [`crate::compress::context::CompressContext`] on the decompress side.
///
/// [`PartitionDecompressor`]: crate::decompress::PartitionDecompressor
pub struct DecompressContext {
    /// Object storage for blobs and manifests.
    pub storage: Arc<dyn Storage>,
    /// Encoder/decoder router.
    pub router: Arc<RouterEncoder>,
    /// Rayon worker count.
    pub workers: usize,
    /// Patch files extracted from the patches tar archive before the partition
    /// loop.  Keyed by archive entry name (e.g. `"000000.patch"`).
    /// Empty when there is no base image.
    pub patch_map: Arc<HashMap<String, Vec<u8>>>,
}
