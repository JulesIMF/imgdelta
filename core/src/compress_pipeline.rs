// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// compress_pipeline: compatibility re-export shim

// All implementation has moved to `crate::compress` and its sub-modules.
// This module is kept for backward compatibility with integration tests and
// external callers that import from `image_delta_core::compress_pipeline`.

#![allow(unused_imports)]

// ── Re-exports ────────────────────────────────────────────────────────────────

pub use crate::compress::compress_fs_partition;
pub use crate::compress::FsDraft;

// Stage functions re-exported under their original (pre-refactor) names.
pub use crate::compress::stages::cleanup::cleanup_fn as cleanup;
pub use crate::compress::stages::compute_patches::compute_patches_fn as compute_patches;
pub use crate::compress::stages::download_blobs::download_blobs_for_patches_fn as download_blobs_for_patches;
pub use crate::compress::stages::match_renamed::match_renamed_fn as match_renamed;
pub use crate::compress::stages::pack_archive::pack_and_upload_archive_fn as pack_and_upload_archive;
pub use crate::compress::stages::s3_lookup::s3_lookup_fn as s3_lookup;
pub use crate::compress::stages::upload_blobs::upload_lazy_blobs_fn as upload_lazy_blobs;
pub use crate::compress::stages::walkdir::walkdir_fn as walkdir;
