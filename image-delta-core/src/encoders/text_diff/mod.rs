use crate::{DeltaEncoder, Error, Result};

/// Delta encoder for text files using a Myers diff algorithm.
///
/// Produces a line-level unified diff patch (via the `diffy` crate) that is
/// human-readable and compresses well with generic compressors.  Suitable for
/// config files, scripts, and other UTF-8 text.
///
/// # Errors
///
/// Both [`encode`] and [`decode`] return [`Error::Encode`] if either the source
/// or target (or the delta) bytes are not valid UTF-8.  Use a binary encoder
/// (e.g. [`Xdelta3Encoder`]) for non-text files.
///
/// [`encode`]: DeltaEncoder::encode
/// [`decode`]: DeltaEncoder::decode
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

impl DeltaEncoder for TextDiffEncoder {
    fn encode(&self, source: &[u8], target: &[u8]) -> Result<Vec<u8>> {
        let src_str = std::str::from_utf8(source)
            .map_err(|_| Error::Encode("source is not valid UTF-8".into()))?;
        let tgt_str = std::str::from_utf8(target)
            .map_err(|_| Error::Encode("target is not valid UTF-8".into()))?;
        let patch = diffy::create_patch(src_str, tgt_str);
        Ok(patch.to_string().into_bytes())
    }

    fn decode(&self, source: &[u8], delta: &[u8]) -> Result<Vec<u8>> {
        let src_str = std::str::from_utf8(source)
            .map_err(|_| Error::Encode("source is not valid UTF-8".into()))?;
        let patch_str = std::str::from_utf8(delta)
            .map_err(|_| Error::Encode("delta is not valid UTF-8".into()))?;
        let patch = diffy::Patch::from_str(patch_str)
            .map_err(|e| Error::Encode(format!("malformed patch: {e}")))?;
        let result = diffy::apply(src_str, &patch)
            .map_err(|e| Error::Encode(format!("patch apply failed: {e}")))?;
        Ok(result.into_bytes())
    }

    fn algorithm_id(&self) -> &'static str {
        "text-diff"
    }
}
