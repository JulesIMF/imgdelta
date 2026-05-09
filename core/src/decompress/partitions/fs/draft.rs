// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Decompress pipeline: intermediate draft state

use crate::decompress::PartitionDecompressStats;

// ── DecompressDraft ───────────────────────────────────────────────────────────

/// Mutable state passed through each decompress pipeline stage.
pub struct DecompressDraft {
    /// Accumulated decompress statistics.
    pub stats: PartitionDecompressStats,
}

impl DecompressDraft {
    /// Create an empty draft with zeroed statistics.
    pub fn new() -> Self {
        Self {
            stats: PartitionDecompressStats::default(),
        }
    }
}

impl Default for DecompressDraft {
    fn default() -> Self {
        Self::new()
    }
}
