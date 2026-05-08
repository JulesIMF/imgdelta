// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Re-exports all built-in PatchEncoder implementations

pub mod algorithm;
pub mod encoders;

pub use encoders::passthrough::PassthroughEncoder;
pub use encoders::router::RouterEncoder;
pub use encoders::text_diff::TextDiffEncoder;
pub use encoders::xdelta3::Xdelta3Encoder;

pub use algorithm::{AlgorithmCode, FilePatch, FileSnapshot};

use crate::Result;

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
