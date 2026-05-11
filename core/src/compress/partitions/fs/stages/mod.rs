// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Compress pipeline: Stage 1 — walkdir

pub mod blob_lookup;
pub mod cleanup;
pub mod compute_patches;
pub mod download_blobs;
pub mod match_renamed;
pub mod pack_archive;
pub mod upload_blobs;
pub mod walkdir;

pub use blob_lookup::BlobLookup;
pub use cleanup::Cleanup;
pub use compute_patches::ComputePatches;
pub use download_blobs::DownloadBlobsForPatches;
pub use match_renamed::MatchRenamed;
pub use pack_archive::PackAndUploadArchive;
pub use upload_blobs::UploadLazyBlobs;
pub use walkdir::Walkdir;

// Stage fn re-exports (canonical location).
pub use blob_lookup::blob_lookup_fn;
pub use cleanup::cleanup_fn;
pub use compute_patches::compute_patches_fn;
pub use download_blobs::download_blobs_for_patches_fn;
pub use match_renamed::match_renamed_fn;
pub use pack_archive::pack_and_upload_archive_fn;
pub use upload_blobs::upload_lazy_blobs_fn;
pub use walkdir::walkdir_fn;
