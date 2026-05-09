// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Decompress pipeline: intermediate draft state

use std::collections::HashMap;
use std::sync::Arc;

use uuid::Uuid;

use crate::decompress::PartitionDecompressStats;

// ── DecompressDraft ───────────────────────────────────────────────────────────

/// Mutable state passed through each decompress pipeline stage.
pub struct DecompressDraft {
    /// Accumulated decompress statistics.
    pub stats: PartitionDecompressStats,
    /// Pre-downloaded blobs keyed by UUID.
    /// Populated by the `DownloadBlobs` stage; read by all record-applying stages.
    pub blob_cache: Arc<HashMap<Uuid, Vec<u8>>>,
}

impl DecompressDraft {
    /// Create an empty draft with zeroed statistics and an empty blob cache.
    pub fn new() -> Self {
        Self {
            stats: PartitionDecompressStats::default(),
            blob_cache: Arc::new(HashMap::new()),
        }
    }
}

impl Default for DecompressDraft {
    fn default() -> Self {
        Self::new()
    }
}
