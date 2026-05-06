// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Compress pipeline: immutable stage context

use std::path::Path;
use std::sync::Arc;

use crate::routing::RouterEncoder;
use crate::storage::Storage;

/// Immutable shared resources passed by reference to every stage `run()` call.
///
/// Created once per partition by [`compress_fs_partition`] and borrowed by all
/// stages.  Storing it as a plain struct (not `Arc`-wrapped) keeps the borrow
/// checker happy: stages receive `&StageContext` which is `Copy`-like to pass
/// around.
///
/// [`compress_fs_partition`]: super::compress_fs_partition
pub struct StageContext {
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
    /// The directory is owned by the caller (`TempDir` in `compress_fs_partition`)
    /// and is guaranteed to outlive all stages.
    pub tmp_dir: Arc<Path>,
}
