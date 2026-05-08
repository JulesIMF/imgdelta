// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Compress pipeline: shared context passed to every stage and compressor

use std::path::Path;
use std::sync::Arc;

use crate::encoding::RouterEncoder;
use crate::storage::Storage;

/// Immutable shared resources passed by reference to every stage and
/// [`PartitionCompressor`] implementation.
///
/// Created once per partition by the orchestrator and borrowed by all stages
/// via `&CompressContext`.  Storing it as a plain struct (not `Arc`-wrapped)
/// keeps the borrow checker happy.
///
/// [`PartitionCompressor`]: crate::compress::partitions::PartitionCompressor
pub struct CompressContext {
    /// Object storage backend (S3, local, fake …).
    pub storage: Arc<dyn Storage>,
    /// Encoder router — selects the appropriate patch algorithm per file.
    pub router: Arc<RouterEncoder>,
    /// Image ID being built.
    pub image_id: Arc<str>,
    /// Base image ID, if this is an incremental (delta) image.
    pub base_image_id: Option<Arc<str>>,
    /// Partition number (1-based) within the image.
    pub partition_number: Option<i32>,
    /// Number of rayon worker threads to use for CPU-bound stages.
    pub workers: usize,
    /// Temporary directory for downloaded blobs (stage 6).
    ///
    /// The directory is owned by the caller and is guaranteed to outlive all
    /// stages.
    pub tmp_dir: Arc<Path>,
    /// Directory where FS stages write per-patch files.
    ///
    /// Each patch is stored as a separate file `<key>` inside this directory.
    /// After all partitions are processed the orchestrator reads this directory
    /// and packs every file into a single tar archive.
    pub patches_dir: Arc<Path>,
    /// Optional directory to dump per-stage debug snapshots into.
    ///
    /// When `Some`, each stage writes a `<NN>_<name>.json` file after running.
    pub debug_dir: Option<Arc<Path>>,
}
