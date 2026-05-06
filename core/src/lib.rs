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

pub mod algorithm;
pub mod compress;
pub mod compress_pipeline;
pub mod compressor;
pub mod decompress;
pub mod decompress_pipeline;
pub mod encoder;
pub mod encoders;
pub mod formats;
pub mod fs_diff;
pub mod image;
pub mod manifest;
pub mod partition;
pub mod path_match;
pub mod routing;
pub mod storage;

// ── Re-exports ────────────────────────────────────────────────────────────────

pub use error::{Error, Result};

pub use algorithm::{AlgorithmCode, FilePatch, FileSnapshot};
pub use compress::FsDraft;
pub use compressor::{
    CompressOptions, CompressionStats, Compressor, DecompressOptions, DecompressionStats,
    DefaultCompressor, DeleteOptions, DeleteStats,
};
pub use encoder::PatchEncoder;
pub use encoders::{PassthroughEncoder, TextDiffEncoder, Xdelta3Encoder};
pub use formats::DirectoryImage;
#[cfg(all(target_os = "linux", feature = "qcow2"))]
pub use formats::Qcow2Image;
pub use image::{
    BiosBootHandle, FsHandle, Image, MountHandle, OpenImage, PartitionHandle, RawHandle,
    SimpleMountHandle,
};
pub use manifest::{
    BlobRef, Data, DataRef, EntryType, Manifest, ManifestHeader, Metadata, PartitionContent,
    PartitionManifest, Patch, PatchRef, Record, MANIFEST_VERSION,
};
pub use partition::{DiskLayout, DiskScheme, PartitionDescriptor, PartitionKind};
pub use routing::{ElfRule, FileInfo, GlobRule, MagicRule, RouterEncoder, RoutingRule, SizeRule};
pub use storage::{BlobCandidate, ImageMeta, ImageStatus, Storage};
