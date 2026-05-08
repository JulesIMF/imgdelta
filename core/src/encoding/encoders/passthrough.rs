// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// PassthroughEncoder: stores files verbatim when delta would be larger

use super::super::PatchAlgorithm;
use super::super::{AlgorithmCode, FilePatch, FileSnapshot, PatchEncoder};
use crate::Result;

// ── PassthroughAlgorithm ──────────────────────────────────────────────────────

/// Raw passthrough algorithm: returns `target` bytes verbatim.
///
/// Zero-size type — instantiation is free.
struct PassthroughAlgorithm;

impl PassthroughAlgorithm {
    fn new() -> Self {
        Self
    }
}

impl PatchAlgorithm for PassthroughAlgorithm {
    fn encode_raw(&self, _source: &[u8], target: &[u8]) -> Result<Vec<u8>> {
        Ok(target.to_vec())
    }

    fn decode_raw(&self, _source: &[u8], patch: &[u8]) -> Result<Vec<u8>> {
        Ok(patch.to_vec())
    }
}

// ── PassthroughEncoder ────────────────────────────────────────────────────────

/// A no-op encoder that stores the target file verbatim.
///
/// Used in two situations:
///
/// 1. **Already-compressed files** (gzip, zstd, PNG, …): patch-encoding these
///    typically produces output larger than the original, so we skip it.
///    Configured via [`MagicRule`] or [`GlobRule`] in the routing table.
///
/// 2. **Patch worse than original**: after encoding, if
///    `patch.len() >= target.len() * threshold` the compressor discards the
///    patch and falls back to `PassthroughEncoder`.
///
/// # Protocol
///
/// - `encode(req)` → returns `req.target` unchanged.
/// - `decode(source, patch)` → returns `patch` unchanged.
/// - `algorithm_id()` → `"passthrough"`.
///
/// [`MagicRule`]: crate::MagicRule
/// [`GlobRule`]: crate::GlobRule
///
/// # Examples
///
/// ```
/// use image_delta_core::{FileSnapshot, PatchEncoder, PassthroughEncoder};
///
/// let enc = PassthroughEncoder::new();
/// let src = b"old content";
/// let tgt = b"new content";
/// let base = FileSnapshot { bytes: src, path: "", size: src.len() as u64, header: &[] };
/// let target = FileSnapshot { bytes: tgt, path: "", size: tgt.len() as u64, header: &[] };
///
/// let patch = enc.encode(&base, &target).unwrap();
/// assert_eq!(&patch.bytes, tgt);
///
/// let restored = enc.decode(src, &patch).unwrap();
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

impl PatchEncoder for PassthroughEncoder {
    fn encode(&self, base: &FileSnapshot<'_>, target: &FileSnapshot<'_>) -> Result<FilePatch> {
        let bytes = PassthroughAlgorithm::new().encode_raw(base.bytes, target.bytes)?;
        Ok(FilePatch::new(bytes, AlgorithmCode::Passthrough))
    }

    fn decode(&self, source: &[u8], patch: &FilePatch) -> Result<Vec<u8>> {
        PassthroughAlgorithm::new().decode_raw(source, &patch.bytes)
    }

    fn algorithm_code(&self) -> Option<AlgorithmCode> {
        Some(AlgorithmCode::Passthrough)
    }

    fn algorithm_id(&self) -> &'static str {
        "passthrough"
    }
}
