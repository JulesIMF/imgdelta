// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Compress pipeline: stage context (re-exported from compress::context)

/// Type alias kept for backward compatibility with all per-stage imports.
///
/// The canonical type is [`crate::compress::context::CompressContext`].
pub use crate::compress::context::CompressContext as StageContext;
