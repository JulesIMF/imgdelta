// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Decompress pipeline: stage context

use std::path::Path;
use std::sync::Arc;

use crate::encoding::RouterEncoder;
use crate::manifest::Record;
use crate::storage::Storage;

// ── DecompressContext ─────────────────────────────────────────────────────────

/// Immutable inputs shared across all decompress pipeline stages.
pub struct DecompressContext {
    /// Object storage for blobs and manifests.
    pub storage: Arc<dyn Storage>,
    /// Encoder/decoder router.
    pub router: Arc<RouterEncoder>,
    /// Rayon worker count.
    pub workers: usize,
    /// Root of the base (previous) filesystem tree.
    pub base_root: Arc<Path>,
    /// Root of the output filesystem tree (written by the pipeline).
    pub output_root: Arc<Path>,
    /// Partition records from the manifest.
    pub records: Arc<[Record]>,
    /// Raw bytes of the patches archive (may be empty).
    pub archive_bytes: Arc<[u8]>,
    /// Whether `archive_bytes` is gzip-compressed.
    pub patches_compressed: bool,
}
