// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// RouterEncoder: selects a PatchEncoder per file path using glob rules

use crate::{AlgorithmCode, FilePatch, FileSnapshot, PatchEncoder, Result};
use std::sync::Arc;

/// Context passed to routing rules for per-file encoder selection.
pub struct FileInfo<'a> {
    /// Path relative to the filesystem root (forward-slash separated).
    pub path: &'a str,
    /// Uncompressed file size in bytes.
    pub size: u64,
    /// First up to 16 bytes of the file content, for magic-byte detection.
    pub header: &'a [u8],
}

/// A single routing rule that maps a file to a [`PatchEncoder`].
///
/// Rules are evaluated in order by [`RouterEncoder`].  The first rule whose
/// [`accept`] returns `true` wins.
///
/// [`accept`]: RoutingRule::accept
pub trait RoutingRule: Send + Sync {
    /// Return `true` if this rule applies to `file`.
    fn accept(&self, file: &FileInfo<'_>) -> bool;

    /// The encoder to use when this rule matches.
    fn encoder(&self) -> Arc<dyn PatchEncoder>;
}

/// A [`PatchEncoder`] that delegates to the first matching [`RoutingRule`],
/// falling back to a default encoder if no rule matches.
///
/// Unlike concrete encoders, `RouterEncoder` does not have a fixed algorithm —
/// `algorithm_code()` returns `None` to reflect this.  Use [`select`] to
/// inspect which encoder would be chosen for a given file, or simply call
/// [`encode`] and let the router dispatch internally.
///
/// [`select`]: RouterEncoder::select
/// [`encode`]: PatchEncoder::encode
pub struct RouterEncoder {
    rules: Vec<Box<dyn RoutingRule>>,
    fallback: Arc<dyn PatchEncoder>,
}

impl RouterEncoder {
    pub fn new(rules: Vec<Box<dyn RoutingRule>>, fallback: Arc<dyn PatchEncoder>) -> Self {
        Self { rules, fallback }
    }

    /// Select the encoder for a given file, evaluating rules in order.
    ///
    /// Prefer calling [`PatchEncoder::encode`] on `RouterEncoder` directly —
    /// that builds a [`FileInfo`] from the [`EncodeRequest`] and calls this
    /// method internally.  Use `select` only when you need to inspect the
    /// chosen encoder without encoding.
    pub fn select(&self, file: &FileInfo<'_>) -> Arc<dyn PatchEncoder> {
        for rule in &self.rules {
            if rule.accept(file) {
                return rule.encoder();
            }
        }
        Arc::clone(&self.fallback)
    }

    /// Find a decoder matching `code` (or `id` when `code == Extended`).
    ///
    /// Checks the fallback encoder and all rule encoders in order.
    /// Returns `None` if no matching encoder is registered.
    pub fn find_decoder(
        &self,
        code: AlgorithmCode,
        id: Option<&str>,
    ) -> Option<Arc<dyn PatchEncoder>> {
        let all = std::iter::once(Arc::clone(&self.fallback))
            .chain(self.rules.iter().map(|r| r.encoder()));
        for enc in all {
            if code == AlgorithmCode::Extended {
                if id.is_some_and(|s| s == enc.algorithm_id()) {
                    return Some(enc);
                }
            } else if enc.algorithm_code() == Some(code) {
                return Some(enc);
            }
        }
        None
    }
}

impl PatchEncoder for RouterEncoder {
    /// Route to the matching encoder based on `target` metadata, then encode.
    ///
    /// Builds a [`FileInfo`] from the target snapshot, calls [`select`] to
    /// pick the right encoder, then delegates to that encoder.
    ///
    /// [`select`]: RouterEncoder::select
    fn encode(&self, base: &FileSnapshot<'_>, target: &FileSnapshot<'_>) -> Result<FilePatch> {
        let file_info = FileInfo {
            path: target.path,
            size: target.size,
            header: target.header,
        };
        self.select(&file_info).encode(base, target)
    }

    /// Decode `patch` by routing to the encoder that produced it.
    ///
    /// Uses `patch.code` (and `patch.algorithm_id` for `Extended`) to select
    /// the concrete decoder, then delegates decoding to it.  No separate
    /// `find_decoder` call is needed by the caller.
    fn decode(&self, source: &[u8], patch: &FilePatch) -> Result<Vec<u8>> {
        let decoder = self
            .find_decoder(patch.code, patch.algorithm_id.as_deref())
            .ok_or_else(|| {
                crate::Error::Decode(format!(
                    "RouterEncoder: no decoder registered for algorithm code {:#04x}{}",
                    patch.code.as_u8(),
                    patch
                        .algorithm_id
                        .as_deref()
                        .map(|id| format!(" (id: {id})"))
                        .unwrap_or_default()
                ))
            })?;
        decoder.decode(source, patch)
    }

    /// Returns `None` — `RouterEncoder` has no single fixed algorithm code.
    fn algorithm_code(&self) -> Option<AlgorithmCode> {
        None
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
    encoder: Arc<dyn PatchEncoder>,
}

impl GlobRule {
    /// Create a new `GlobRule` that routes files matching `pattern` to `encoder`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Format`] if `pattern` is not a valid glob.
    pub fn new(pattern: &str, encoder: Arc<dyn PatchEncoder>) -> Result<Self> {
        let pattern =
            glob::Pattern::new(pattern).map_err(|e| crate::Error::Format(e.to_string()))?;
        Ok(Self { pattern, encoder })
    }
}

impl RoutingRule for GlobRule {
    fn accept(&self, file: &FileInfo<'_>) -> bool {
        self.pattern.matches(file.path)
    }

    fn encoder(&self) -> Arc<dyn PatchEncoder> {
        Arc::clone(&self.encoder)
    }
}

/// Route ELF binaries (detected by magic bytes `\x7fELF`).
pub struct ElfRule {
    encoder: Arc<dyn PatchEncoder>,
}

impl ElfRule {
    /// Create a new `ElfRule` that routes ELF binaries to `encoder`.
    pub fn new(encoder: Arc<dyn PatchEncoder>) -> Self {
        Self { encoder }
    }
}

impl RoutingRule for ElfRule {
    fn accept(&self, file: &FileInfo<'_>) -> bool {
        file.header.starts_with(b"\x7fELF")
    }

    fn encoder(&self) -> Arc<dyn PatchEncoder> {
        Arc::clone(&self.encoder)
    }
}

/// Route files smaller than a size threshold.
pub struct SizeRule {
    max_bytes: u64,
    encoder: Arc<dyn PatchEncoder>,
}

impl SizeRule {
    /// Create a new `SizeRule` that routes files up to `max_bytes` in size to `encoder`.
    pub fn new(max_bytes: u64, encoder: Arc<dyn PatchEncoder>) -> Self {
        Self { max_bytes, encoder }
    }
}

impl RoutingRule for SizeRule {
    fn accept(&self, file: &FileInfo<'_>) -> bool {
        file.size <= self.max_bytes
    }

    fn encoder(&self) -> Arc<dyn PatchEncoder> {
        Arc::clone(&self.encoder)
    }
}

/// Route files whose content starts with a specific magic byte sequence.
///
/// Useful for already-compressed formats: PNG (`\x89PNG`), gzip (`\x1f\x8b`), etc.
pub struct MagicRule {
    magic: Vec<u8>,
    encoder: Arc<dyn PatchEncoder>,
}

impl MagicRule {
    /// Create a new `MagicRule` that routes files starting with `magic` bytes to `encoder`.
    pub fn new(magic: impl Into<Vec<u8>>, encoder: Arc<dyn PatchEncoder>) -> Self {
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

    fn encoder(&self) -> Arc<dyn PatchEncoder> {
        Arc::clone(&self.encoder)
    }
}
