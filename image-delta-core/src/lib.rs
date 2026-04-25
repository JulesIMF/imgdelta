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
//! ImageFormat::mount()  →  diff_dirs()  →  path_match()  →  RouterEncoder::select()
//!                                                         →  DeltaEncoder::encode()
//!                                                         →  Storage::upload_blob()
//! ```
//!
//! The library crate contains all algorithm implementations and trait
//! definitions.  The binary crate (`image-delta-cli`) adds the S3/PostgreSQL
//! [`Storage`] implementation and the CLI entry point.

mod error;

pub mod compressor;
pub mod encoder;
pub mod encoders;
pub mod format;
pub mod formats;
pub mod fs_diff;
pub mod manifest;
pub mod path_match;
pub mod routing;
pub mod storage;

pub(crate) mod scheduler;

// ── Re-exports ────────────────────────────────────────────────────────────────

pub use error::{Error, Result};

pub use compressor::{
    CompressOptions, CompressionStats, Compressor, DecompressOptions, DecompressionStats,
    DefaultCompressor,
};
pub use encoder::DeltaEncoder;
pub use encoders::{PassthroughEncoder, TextDiffEncoder, Xdelta3Encoder};
pub use format::{ImageFormat, MountHandle, SimpleMountHandle};
pub use formats::DirectoryFormat;
#[cfg(feature = "qcow2")]
pub use formats::Qcow2Format;
pub use manifest::{BlobRef, DeltaRef, Entry, EntryType, Manifest, ManifestHeader};
pub use routing::{ElfRule, FileInfo, GlobRule, MagicRule, RouterEncoder, RoutingRule, SizeRule};
pub use storage::{BlobCandidate, ImageMeta, ImageStatus, Storage};
