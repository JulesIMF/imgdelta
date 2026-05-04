// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// TextDiffEncoder: unified-diff based encoder for human-readable text files

use crate::encoder::PatchAlgorithm;
use crate::{AlgorithmCode, Error, FilePatch, FileSnapshot, PatchEncoder, Result};

// ── MyersAlgorithm ────────────────────────────────────────────────────────────

/// Raw Myers line-diff algorithm (via the `diffy` crate).
///
/// Produces a unified-diff patch that is human-readable and compresses well.
/// Zero-size type — instantiation is free.
///
/// # Errors
///
/// Both [`encode_raw`] and [`decode_raw`] return [`Error::Encode`] / [`Error::Decode`]
/// if the bytes are not valid UTF-8.
///
/// [`encode_raw`]: MyersAlgorithm::encode_raw
/// [`decode_raw`]: MyersAlgorithm::decode_raw
pub(crate) struct MyersAlgorithm;

impl MyersAlgorithm {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl PatchAlgorithm for MyersAlgorithm {
    fn encode_raw(&self, source: &[u8], target: &[u8]) -> Result<Vec<u8>> {
        let src_str = std::str::from_utf8(source)
            .map_err(|_| Error::Encode("source is not valid UTF-8".into()))?;
        let tgt_str = std::str::from_utf8(target)
            .map_err(|_| Error::Encode("target is not valid UTF-8".into()))?;
        Ok(diffy::create_patch(src_str, tgt_str)
            .to_string()
            .into_bytes())
    }

    fn decode_raw(&self, source: &[u8], patch: &[u8]) -> Result<Vec<u8>> {
        let src_str = std::str::from_utf8(source)
            .map_err(|_| Error::Decode("source is not valid UTF-8".into()))?;
        let patch_str = std::str::from_utf8(patch)
            .map_err(|_| Error::Decode("patch is not valid UTF-8".into()))?;
        let patch_obj = diffy::Patch::from_str(patch_str)
            .map_err(|e| Error::Decode(format!("malformed patch: {e}")))?;
        let result = diffy::apply(src_str, &patch_obj)
            .map_err(|e| Error::Decode(format!("patch apply failed: {e}")))?;
        Ok(result.into_bytes())
    }
}

// ── TextDiffEncoder ───────────────────────────────────────────────────────────

/// Patch encoder for text files using the Myers line-diff algorithm.
///
/// Delegates to [`MyersAlgorithm`] per encode/decode call.
/// Zero-size type — instantiation is free.
///
/// Produces a line-level unified diff patch (via the `diffy` crate) that is
/// human-readable and compresses well with generic compressors.  Suitable for
/// config files, scripts, and other UTF-8 text.
///
/// # Errors
///
/// Both [`encode`] and [`decode`] return an error if either the source,
/// target, or patch bytes are not valid UTF-8.  Use a binary encoder
/// (e.g. [`Xdelta3Encoder`]) for non-text files.
///
/// [`encode`]: PatchEncoder::encode
/// [`decode`]: PatchEncoder::decode
/// [`Xdelta3Encoder`]: crate::Xdelta3Encoder
pub struct TextDiffEncoder;

impl TextDiffEncoder {
    pub fn new() -> Self {
        Self
    }
}

impl Default for TextDiffEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl PatchEncoder for TextDiffEncoder {
    fn encode(&self, base: &FileSnapshot<'_>, target: &FileSnapshot<'_>) -> Result<FilePatch> {
        let bytes = MyersAlgorithm::new().encode_raw(base.bytes, target.bytes)?;
        Ok(FilePatch::new(bytes, AlgorithmCode::TextDiff))
    }

    fn decode(&self, source: &[u8], patch: &FilePatch) -> Result<Vec<u8>> {
        MyersAlgorithm::new().decode_raw(source, &patch.bytes)
    }

    fn algorithm_code(&self) -> Option<AlgorithmCode> {
        Some(AlgorithmCode::TextDiff)
    }

    fn algorithm_id(&self) -> &'static str {
        "text-diff"
    }
}
