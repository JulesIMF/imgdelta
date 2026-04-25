use crate::{DeltaEncoder, Result};

/// Delta encoder for text files using a Myers diff algorithm.
///
/// Produces a line-level patch (unified diff format) that is human-readable
/// and compresses well with standard compressors.  Suitable for config files,
/// scripts, and other UTF-8 text.
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
    fn encode(&self, _source: &[u8], _target: &[u8]) -> Result<Vec<u8>> {
        todo!("Phase 2: Myers diff (pure Rust)")
    }

    fn decode(&self, _source: &[u8], _delta: &[u8]) -> Result<Vec<u8>> {
        todo!("Phase 2: apply unified diff patch")
    }

    fn algorithm_id(&self) -> &'static str {
        "text-diff"
    }
}
