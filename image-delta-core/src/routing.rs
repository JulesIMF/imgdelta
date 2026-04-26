use crate::{DeltaEncoder, Entry, Result};
use std::sync::Arc;

/// Context passed to routing rules so they can inspect file metadata and
/// raw bytes without loading the full file content.
pub struct FileInfo<'a> {
    pub entry: &'a Entry,
    /// First up to 16 bytes of the file content, for magic-byte detection.
    pub header: &'a [u8],
}

/// A single routing rule that maps a file to a [`DeltaEncoder`].
///
/// Rules are evaluated in order by [`RouterEncoder`].  The first rule whose
/// [`accept`] returns `true` wins.
///
/// [`accept`]: RoutingRule::accept
pub trait RoutingRule: Send + Sync {
    /// Return `true` if this rule applies to `file`.
    fn accept(&self, file: &FileInfo<'_>) -> bool;

    /// The encoder to use when this rule matches.
    fn encoder(&self) -> Arc<dyn DeltaEncoder>;
}

/// A [`DeltaEncoder`] that delegates to the first matching [`RoutingRule`],
/// falling back to a default encoder if no rule matches.
///
/// Note: `RouterEncoder::encode` and `::decode` require `FileInfo` context that
/// is not available through the raw `DeltaEncoder` interface.  The compressor
/// uses [`RouterEncoder::select`] directly; the `DeltaEncoder` impl is provided
/// for trait-object compatibility only.
pub struct RouterEncoder {
    rules: Vec<Box<dyn RoutingRule>>,
    fallback: Arc<dyn DeltaEncoder>,
}

impl RouterEncoder {
    pub fn new(rules: Vec<Box<dyn RoutingRule>>, fallback: Arc<dyn DeltaEncoder>) -> Self {
        Self { rules, fallback }
    }

    /// Select the encoder for a given file, evaluating rules in order.
    pub fn select(&self, file: &FileInfo<'_>) -> Arc<dyn DeltaEncoder> {
        for rule in &self.rules {
            if rule.accept(file) {
                return rule.encoder();
            }
        }
        Arc::clone(&self.fallback)
    }
}

impl DeltaEncoder for RouterEncoder {
    fn encode(&self, _source: &[u8], _target: &[u8]) -> Result<Vec<u8>> {
        // RouterEncoder::encode is not called directly; use select() + encode().
        unimplemented!("use RouterEncoder::select() to obtain the concrete encoder first")
    }

    fn decode(&self, _source: &[u8], _delta: &[u8]) -> Result<Vec<u8>> {
        // Decoding is dispatched by algorithm_id stored in the manifest, not by
        // routing rules — so this path is never reached in production.
        unimplemented!("decoder is selected by algorithm_id from the manifest, not by routing")
    }

    fn algorithm_id(&self) -> &'static str {
        "router"
    }
}

// ── Built-in routing rules ────────────────────────────────────────────────────

/// Route files whose path matches a glob pattern.
///
/// # Examples
///
/// Route all `*.gz` files to [`PassthroughEncoder`]:
///
/// ```
/// use std::sync::Arc;
/// use image_delta_core::{GlobRule, PassthroughEncoder};
///
/// let rule = GlobRule::new("**/*.gz", Arc::new(PassthroughEncoder::new())).unwrap();
/// ```
///
/// [`PassthroughEncoder`]: crate::PassthroughEncoder
pub struct GlobRule {
    pattern: glob::Pattern,
    encoder: Arc<dyn DeltaEncoder>,
}

impl GlobRule {
    /// Create a new `GlobRule` that routes files matching `pattern` to `encoder`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Format`] if `pattern` is not a valid glob.
    pub fn new(pattern: &str, encoder: Arc<dyn DeltaEncoder>) -> Result<Self> {
        let pattern =
            glob::Pattern::new(pattern).map_err(|e| crate::Error::Format(e.to_string()))?;
        Ok(Self { pattern, encoder })
    }
}

impl RoutingRule for GlobRule {
    fn accept(&self, file: &FileInfo<'_>) -> bool {
        self.pattern.matches(&file.entry.path)
    }

    fn encoder(&self) -> Arc<dyn DeltaEncoder> {
        Arc::clone(&self.encoder)
    }
}

/// Route ELF binaries (detected by magic bytes `\x7fELF`).
pub struct ElfRule {
    encoder: Arc<dyn DeltaEncoder>,
}

impl ElfRule {
    /// Create a new `ElfRule` that routes ELF binaries to `encoder`.
    pub fn new(encoder: Arc<dyn DeltaEncoder>) -> Self {
        Self { encoder }
    }
}

impl RoutingRule for ElfRule {
    fn accept(&self, file: &FileInfo<'_>) -> bool {
        file.header.starts_with(b"\x7fELF")
    }

    fn encoder(&self) -> Arc<dyn DeltaEncoder> {
        Arc::clone(&self.encoder)
    }
}

/// Route files smaller than a size threshold.
pub struct SizeRule {
    max_bytes: u64,
    encoder: Arc<dyn DeltaEncoder>,
}

impl SizeRule {
    /// Create a new `SizeRule` that routes files up to `max_bytes` in size to `encoder`.
    pub fn new(max_bytes: u64, encoder: Arc<dyn DeltaEncoder>) -> Self {
        Self { max_bytes, encoder }
    }
}

impl RoutingRule for SizeRule {
    fn accept(&self, file: &FileInfo<'_>) -> bool {
        file.entry.size <= self.max_bytes
    }

    fn encoder(&self) -> Arc<dyn DeltaEncoder> {
        Arc::clone(&self.encoder)
    }
}

/// Route files whose content starts with a specific magic byte sequence.
///
/// Useful for already-compressed formats: PNG (`\x89PNG`), gzip (`\x1f\x8b`), etc.
pub struct MagicRule {
    magic: Vec<u8>,
    encoder: Arc<dyn DeltaEncoder>,
}

impl MagicRule {
    /// Create a new `MagicRule` that routes files starting with `magic` bytes to `encoder`.
    pub fn new(magic: impl Into<Vec<u8>>, encoder: Arc<dyn DeltaEncoder>) -> Self {
        Self {
            magic: magic.into(),
            encoder,
        }
    }
}

impl RoutingRule for MagicRule {
    fn accept(&self, file: &FileInfo<'_>) -> bool {
        file.header.starts_with(&self.magic)
    }

    fn encoder(&self) -> Arc<dyn DeltaEncoder> {
        Arc::clone(&self.encoder)
    }
}
