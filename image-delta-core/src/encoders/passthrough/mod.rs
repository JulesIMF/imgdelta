use crate::{DeltaEncoder, Result};

/// A no-op encoder that stores the target file verbatim.
///
/// Used in two situations:
///
/// 1. **Already-compressed files** (gzip, zstd, PNG, …): delta-encoding these
///    typically produces output larger than the original, so we skip it.
///    Configured via [`MagicRule`] or [`GlobRule`] in the routing table.
///
/// 2. **Delta worse than original**: after encoding, if
///    `delta.len() >= target.len() * threshold` the compressor discards the
///    delta and falls back to `PassthroughEncoder`.
///
/// # Protocol
///
/// - `encode(source, target)` → returns `target` unchanged (ignores `source`).
/// - `decode(source, delta)` → returns `delta` unchanged (ignores `source`).
/// - `algorithm_id()` → `"passthrough"`.
///
/// [`MagicRule`]: crate::MagicRule
/// [`GlobRule`]: crate::GlobRule
///
/// # Examples
///
/// ```
/// use image_delta_core::{DeltaEncoder, PassthroughEncoder};
///
/// let enc = PassthroughEncoder::new();
/// let src = b"old content";
/// let tgt = b"new content";
///
/// let delta = enc.encode(src, tgt).unwrap();
/// assert_eq!(&delta, tgt);
///
/// let restored = enc.decode(src, &delta).unwrap();
/// assert_eq!(&restored, tgt);
/// ```
pub struct PassthroughEncoder;

impl PassthroughEncoder {
    pub fn new() -> Self {
        Self
    }
}

impl Default for PassthroughEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl DeltaEncoder for PassthroughEncoder {
    fn encode(&self, _source: &[u8], target: &[u8]) -> Result<Vec<u8>> {
        Ok(target.to_vec())
    }

    fn decode(&self, _source: &[u8], delta: &[u8]) -> Result<Vec<u8>> {
        Ok(delta.to_vec())
    }

    fn algorithm_id(&self) -> &'static str {
        "passthrough"
    }
}
