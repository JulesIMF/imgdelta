// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Compress pipeline: module root and public entry point

//! Per-partition compress pipeline organised under [`compress::partitions`].
//!
//! The public entry point for FS partitions is
//! [`compress_fs_partition`][partitions::fs::compress_fs_partition], re-exported
//! here for backward compatibility.

pub mod partitions;

// Re-export the FS draft type used by the CLI and tests.
pub use partitions::fs::FsDraft;

// Re-export entry point so existing callers (`crate::compress::compress_fs_partition`)
// keep working without path changes.
pub use partitions::fs::compress_fs_partition;
