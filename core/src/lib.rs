// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Crate root: public re-exports and feature flags

//! # image-delta-core
//!
//! Core library for per-file delta compression of cloud OS images.
//!
//! Provides traits and implementations for compressing filesystem snapshots
//! into binary deltas, storing patches in object storage, and reconstructing
//! images offline.
//!
//! ## Architecture
//!
//! ```text
//! Image::mount()  →  diff_dirs()  →  path_match()  →  RouterEncoder::encode(EncodeRequest)
//!                                                         →  Storage::upload_blob()
//! ```
//!
//! The library crate contains all algorithm implementations and trait
//! definitions.  The binary crate (`image-delta-cli`) adds the S3/PostgreSQL
//! [`Storage`] implementation and the CLI entry point.

mod error;

pub mod compress;
pub mod compressor;
pub mod decompress;
pub mod encoding;
pub mod fs_diff;
pub mod image;
pub mod manifest;
pub mod partitions;
pub mod path_match;
pub mod storage;

// ── Re-exports ────────────────────────────────────────────────────────────────

pub use error::{Error, Result};

pub use compressor::{
    CompressOptions, CompressionStats, Compressor, DecompressOptions, DecompressionStats,
    DefaultCompressor, DeleteOptions, DeleteStats,
};
pub use encoding::router::{
    ElfRule, FileInfo, GlobRule, MagicRule, RouterEncoder, RoutingRule, SizeRule,
};
pub use image::DirectoryImage;
#[cfg(all(target_os = "linux", feature = "qcow2"))]
pub use image::Qcow2Image;
pub use image::{Image, OpenImage};
pub use manifest::{
    BlobRef, Data, DataRef, EntryType, Manifest, ManifestHeader, Metadata, PartitionContent,
    PartitionManifest, Patch, PatchRef, Record, MANIFEST_VERSION,
};
pub use partitions::{
    BiosBootHandle, FsHandle, MbrHandle, MountHandle, PartitionHandle, RawHandle, SimpleMountHandle,
};
pub use partitions::{DiskLayout, DiskScheme, PartitionDescriptor, PartitionKind};
pub use storage::{BlobCandidate, ImageMeta, ImageStatus, Storage};
