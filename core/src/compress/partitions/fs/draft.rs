// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// FsDraft: mutable working state threaded through compress pipeline stages

use std::collections::HashMap;
use std::path::PathBuf;

use crate::manifest::Record;

// ── FsDraft ───────────────────────────────────────────────────────────────────

/// Mutable working state passed through the compress pipeline stages.
///
/// After [`pack_and_upload_archive`] the draft is consumed and a
/// [`PartitionContent::Fs`] is returned.
///
/// [`pack_and_upload_archive`]: crate::compress::stages::pack_archive::pack_and_upload_archive_fn
#[derive(Debug, Default)]
pub struct FsDraft {
    /// File-level change records for this partition.
    ///
    /// Progressively refined across stages:
    /// - Stage 1: raw `Patch::Lazy` / `Data::LazyBlob` / `Data::OriginalFile`
    /// - Stage 5: `LazyBlob` → `BlobRef`
    /// - Stage 6: `DataRef::BlobRef` in patches → `DataRef::FilePath`
    /// - Stage 7: `Patch::Lazy` → `Patch::Real`
    pub records: Vec<Record>,

    /// SHA-256 hashes for **base-image** regular files, keyed by relative path
    /// (same key space as `old_path` on removed records).
    ///
    /// Used by `match_renamed` (Pass 1) to identify pure-path renames — files
    /// where the content is identical but the path changed.
    pub base_hashes: HashMap<String, [u8; 32]>,

    /// SHA-256 hashes for **target-image** regular files, keyed by relative
    /// path (same key space as `new_path` on added records).
    pub target_hashes: HashMap<String, [u8; 32]>,

    /// Temporary files downloaded from storage for patch computation.
    ///
    /// All paths in this list must be removed by the caller after
    /// [`compute_patches`] completes.
    ///
    /// [`compute_patches`]: crate::compress::stages::compute_patches::compute_patches_fn
    pub tmp_files: Vec<PathBuf>,

    /// Raw patch bytes indexed by archive-entry name.
    ///
    /// Populated by [`compute_patches`], consumed (and cleared) by
    /// [`pack_and_upload_archive`].
    pub patch_bytes: HashMap<String, Vec<u8>>,

    /// Pre-computed SHA-256 hashes for `Data::LazyBlob` files, keyed by the
    /// absolute host path stored in the `LazyBlob` variant.
    ///
    /// Populated by the walkdir stage so that subsequent stages
    /// (in particular `upload_lazy_blobs`) can skip re-reading files from
    /// the NBD mount just to compute a content hash they already have.
    pub blob_sha256: HashMap<PathBuf, [u8; 32]>,
}
