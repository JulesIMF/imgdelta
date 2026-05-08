// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Re-exports all built-in PatchEncoder implementations

pub mod encoders;

pub mod passthrough;
pub mod router;
pub mod text_diff;
pub mod xdelta3;

pub use passthrough::PassthroughEncoder;
pub use router::RouterEncoder;
pub use text_diff::TextDiffEncoder;
pub use xdelta3::Xdelta3Encoder;

use crate::Result;
use num_enum::TryFromPrimitive;
use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

// ── PatchAlgorithm ────────────────────────────────────────────────────────────

/// Low-level encoding/decoding algorithm.
///
/// This trait is the raw worker — it receives only bytes, applies the
/// algorithm, and returns bytes.  It knows nothing about file metadata,
/// routing, or manifest bookkeeping.
///
/// # Implementing a new algorithm
///
/// 1. Create a new `pub(crate)` struct inside the relevant `encoders/` module.
/// 2. Implement this trait for it.
/// 3. Wrap it with a public `PatchEncoder` implementation that owns an instance
///    (or creates one per call) and maps the algorithm code.
///
/// # Instantiation cost
///
/// Implementors should make `new()` as cheap as possible: a [`PatchEncoder`]
/// may create a fresh algorithm instance for **every file** it encodes.
/// Zero-size types (`struct Xdelta3Algorithm;`) are ideal.
pub(crate) trait PatchAlgorithm {
    /// Encode `target` relative to `source`, returning raw patch bytes.
    fn encode_raw(&self, source: &[u8], target: &[u8]) -> Result<Vec<u8>>;
    /// Reconstruct `target` from `source` and previously produced patch bytes.
    fn decode_raw(&self, source: &[u8], patch: &[u8]) -> Result<Vec<u8>>;
}

/// One-byte code identifying the file-level patch algorithm stored in a [`PatchRef`].
///
/// Using a fixed-width code instead of a string saves 6–15 bytes per manifest
/// entry.  [`AlgorithmCode::Extended`] (`0xFF`) signals that the numeric code
/// space is exhausted and the string `algorithm_id` field in [`PatchRef`] must
/// be consulted instead.  No built-in algorithm currently uses `Extended`.
///
/// Values `0x03..=0xFE` are reserved for future built-in algorithms.
///
/// [`PatchRef`]: crate::PatchRef
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, TryFromPrimitive)]
#[repr(u8)]
pub enum AlgorithmCode {
    /// No compression — target bytes stored verbatim (`"passthrough"`).
    Passthrough = 0x00,
    /// VCDIFF encoding via xdelta3 (`"xdelta3"`).
    Xdelta3 = 0x01,
    /// Myers line-level diff via the `diffy` crate (`"text-diff"`).
    TextDiff = 0x02,
    // 0x03..=0xFE reserved for future built-in algorithms
    /// Code space exhausted — consult the string `algorithm_id` in [`PatchRef`].
    Extended = 0xFF,
}

impl AlgorithmCode {
    /// Return the raw byte value of this code.
    #[inline]
    pub fn as_u8(self) -> u8 {
        self as u8
    }
}

impl Serialize for AlgorithmCode {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_u8(self.as_u8())
    }
}

impl<'de> Deserialize<'de> for AlgorithmCode {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let v = u8::deserialize(d)?;
        Self::try_from(v)
            .map_err(|e| de::Error::custom(format!("unknown algorithm code: {:#04x}", e.number)))
    }
}

// ── FileSnapshot ──────────────────────────────────────────────────────────────

/// A snapshot of a single file: its identity metadata and raw bytes.
///
/// [`PatchEncoder::encode`] receives **two** `FileSnapshot`s — one for the
/// base version and one for the target version — so both sides are described
/// symmetrically with the same struct.
///
/// Routing encoders (e.g. [`RouterEncoder`]) use `path`, `size`, and `header`
/// from the **target** snapshot to select the right algorithm.  Concrete
/// encoders (e.g. [`Xdelta3Encoder`]) use only the `bytes` field from each
/// snapshot and ignore the routing fields.
///
/// [`PatchEncoder::encode`]: crate::PatchEncoder::encode
/// [`RouterEncoder`]: crate::RouterEncoder
/// [`Xdelta3Encoder`]: crate::Xdelta3Encoder
pub struct FileSnapshot<'a> {
    /// File path relative to the image root, forward-slash separated.
    ///
    /// Used by glob-based routing rules.
    pub path: &'a str,
    /// Uncompressed file size in bytes.
    ///
    /// Used by size-based routing rules.
    pub size: u64,
    /// First bytes of the file content (up to 16 bytes), for magic/ELF detection.
    ///
    /// Used by magic-byte and ELF routing rules.
    pub header: &'a [u8],
    /// Raw file content.
    pub bytes: &'a [u8],
}

// ── FilePatch ─────────────────────────────────────────────────────────────────

/// The result of [`PatchEncoder::encode`]: patch bytes together with the
/// algorithm that produced them.
///
/// Bundling the algorithm with the bytes means callers—including
/// [`RouterEncoder`]—can build a correct [`PatchRef`] without a second call
/// to the encoder that was actually selected.
///
/// [`PatchEncoder::encode`]: crate::PatchEncoder::encode
/// [`RouterEncoder`]: crate::RouterEncoder
/// [`PatchRef`]: crate::PatchRef
#[derive(Debug)]
pub struct FilePatch {
    /// Raw patch bytes to archive and later pass to [`PatchEncoder::decode`].
    ///
    /// [`PatchEncoder::decode`]: crate::PatchEncoder::decode
    pub bytes: Vec<u8>,
    /// Compact one-byte algorithm code.
    pub code: AlgorithmCode,
    /// Human-readable algorithm id — `Some` only when
    /// `code == AlgorithmCode::Extended`.
    pub algorithm_id: Option<String>,
}

impl FilePatch {
    /// Construct a `FilePatch` for a built-in algorithm (code ≠ `Extended`).
    ///
    /// # Panics (debug only)
    ///
    /// Debug-asserts that `code != AlgorithmCode::Extended`.
    /// Use [`FilePatch::extended`] for extended algorithms.
    pub fn new(bytes: Vec<u8>, code: AlgorithmCode) -> Self {
        debug_assert_ne!(
            code,
            AlgorithmCode::Extended,
            "use FilePatch::extended for extended algorithms"
        );
        Self {
            bytes,
            code,
            algorithm_id: None,
        }
    }

    /// Construct a `FilePatch` for an extended (non-built-in) algorithm.
    pub fn extended(bytes: Vec<u8>, algorithm_id: impl Into<String>) -> Self {
        Self {
            bytes,
            code: AlgorithmCode::Extended,
            algorithm_id: Some(algorithm_id.into()),
        }
    }
}

// ── PatchEncoder ──────────────────────────────────────────────────────────────

/// Trait for computing and applying file-level binary patches.
///
/// In this project, "patch" refers to a file-level diff (what this trait
/// produces), while "delta" refers to the full image-level diff produced by
/// [`DefaultCompressor`].
///
/// An implementor receives two [`FileSnapshot`]s — one for the base version
/// and one for the target version.  Both are described with the same struct,
/// each carrying file-level metadata (`path`, `size`, `header`) and raw bytes.
/// Concrete encoders (e.g. [`Xdelta3Encoder`]) use only the `bytes` fields.
/// Routing encoders (e.g. [`RouterEncoder`]) use the target metadata to
/// dispatch to the right concrete encoder before delegating.
///
/// `decode` receives the source bytes and a [`FilePatch`] (which carries both
/// the raw patch bytes and the algorithm code), so routing encoders can select
/// the correct decoder without a separate lookup call from the caller.
///
/// # Contract for implementors
///
/// - `decode(base.bytes, encode(base, target)?) == target.bytes` for all inputs.
/// - Implementations must be `Send + Sync` (used from multiple worker threads).
/// - `algorithm_code()` must be stable across versions — it is the primary key
///   stored in the manifest for decoder selection during decompression.
///   Return `None` only for routing/delegating encoders that have no fixed algorithm.
/// - `algorithm_id()` must also be stable — it is the fallback key when
///   `algorithm_code() == Some(AlgorithmCode::Extended)`.
///
/// [`DefaultCompressor`]: crate::DefaultCompressor
/// [`Xdelta3Encoder`]: crate::Xdelta3Encoder
/// [`RouterEncoder`]: crate::RouterEncoder
pub trait PatchEncoder: Send + Sync {
    /// Compute a binary patch from `base` to `target`.
    ///
    /// Both `base` and `target` carry the same metadata fields; routing
    /// encoders use `target.path`, `target.size`, and `target.header` to
    /// select the right algorithm.  Concrete encoders use `base.bytes` and
    /// `target.bytes` only.
    ///
    /// Returns a [`FilePatch`] that bundles the patch bytes with the algorithm
    /// that produced them, so callers can construct a correct [`PatchRef`]
    /// without a second call to the encoder.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Encode`] if encoding fails.
    ///
    /// [`PatchRef`]: crate::PatchRef
    fn encode(&self, base: &FileSnapshot<'_>, target: &FileSnapshot<'_>) -> Result<FilePatch>;

    /// Reconstruct the target from `source` bytes and a previously computed [`FilePatch`].
    ///
    /// The [`FilePatch`] carries both the raw patch bytes (`patch.bytes`) and
    /// the algorithm code (`patch.code`), which routing encoders use to select
    /// the correct concrete decoder without a separate lookup call.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Decode`] if the patch is invalid or corrupt.
    fn decode(&self, source: &[u8], patch: &FilePatch) -> Result<Vec<u8>>;

    /// Compact one-byte algorithm code — the primary lookup key during decompression.
    ///
    /// Returns `None` for routing/delegating encoders (e.g. [`RouterEncoder`])
    /// that do not have a single fixed algorithm.
    ///
    /// Returns `Some(AlgorithmCode::Extended)` for non-built-in algorithms, in
    /// which case [`algorithm_id`] is used as the fallback.
    ///
    /// [`algorithm_id`]: PatchEncoder::algorithm_id
    /// [`RouterEncoder`]: crate::RouterEncoder
    fn algorithm_code(&self) -> Option<AlgorithmCode>;

    /// Stable string identifier.
    ///
    /// Stored in [`PatchRef`] only when
    /// `algorithm_code() == Some(AlgorithmCode::Extended)`.  For built-in
    /// algorithms this string is informational only.
    ///
    /// [`PatchRef`]: crate::PatchRef
    fn algorithm_id(&self) -> &'static str;
}
