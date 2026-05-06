// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// decompress_pipeline: compatibility re-export shim

// All implementation has moved to `crate::decompress` and its sub-modules.
// This module is kept for backward compatibility with external callers that
// import from `image_delta_core::decompress_pipeline`.

// ── Re-exports ────────────────────────────────────────────────────────────────

pub use crate::decompress::decompress_fs_partition;
pub use crate::decompress::PartitionDecompressStats;
