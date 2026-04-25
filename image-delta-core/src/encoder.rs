use crate::Result;

/// Trait for computing and applying binary deltas between file versions.
///
/// Implementors encode a `target` relative to a `source`, producing a compact
/// delta that can later be decoded (with the same `source`) to reconstruct
/// `target` exactly.
///
/// # Contract for implementors
///
/// - `decode(source, encode(source, target)?) == target` must hold for all inputs.
/// - Implementations must be `Send + Sync` (used from multiple worker threads).
/// - `algorithm_id` must be stable across versions: it is stored in the manifest
///   and used to select the correct decoder at decompression time.
pub trait DeltaEncoder: Send + Sync {
    /// Compute a binary delta from `source` to `target`.
    ///
    /// Returns the delta bytes. The delta is meaningless without the corresponding
    /// `source`; store both the delta and a reference to the source blob.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Encode`] if encoding fails.
    fn encode(&self, source: &[u8], target: &[u8]) -> Result<Vec<u8>>;

    /// Reconstruct `target` from `source` and a previously computed `delta`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Decode`] if the delta is invalid or corrupt.
    fn decode(&self, source: &[u8], delta: &[u8]) -> Result<Vec<u8>>;

    /// Stable identifier stored in the manifest.
    ///
    /// Examples: `"xdelta3"`, `"text-diff"`, `"passthrough"`.
    fn algorithm_id(&self) -> &'static str;
}
