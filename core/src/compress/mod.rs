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
