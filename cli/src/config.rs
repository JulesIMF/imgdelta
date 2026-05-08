// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Configuration file loading and validation (TOML → typed structs)

// Config structs are wired in Phase 5/6 CLI commands.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::Deserialize;

use image_delta_core::encoding::{
    PassthroughEncoder, PatchEncoder, RouterEncoder, TextDiffEncoder, Xdelta3Encoder,
};
use image_delta_core::{ElfRule, GlobRule, MagicRule, RoutingRule, SizeRule, Storage};

use crate::impls::local_storage::LocalStorage;
use crate::impls::s3_storage::S3Storage;

/// Which delta encoder to use (fixed set — no runtime string lookup).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EncoderKind {
    Xdelta3,
    TextDiff,
    Passthrough,
}

/// A single entry in the `[[routing]]` TOML array.
///
/// Rules are evaluated in order; the first match wins.
///
/// ```toml
/// [[routing]]
/// type = "glob"
/// pattern = "**/*.gz"
/// encoder = "passthrough"
///
/// [[routing]]
/// type = "elf"
/// encoder = "xdelta3"
/// ```
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RoutingRuleConfig {
    Glob {
        pattern: String,
        encoder: EncoderKind,
    },
    Elf {
        encoder: EncoderKind,
    },
    Size {
        max_bytes: u64,
        encoder: EncoderKind,
    },
    Magic {
        /// Hex-encoded magic bytes, e.g. `"89504e47"` for PNG.
        hex: String,
        encoder: EncoderKind,
    },
}

/// Logging configuration block.
///
/// ```toml
/// [logging]
/// level = "info"
/// file = "/var/log/imgdelta/compress.log"  # optional
///
/// [logging.targets]
/// "image_delta_core::fs_diff" = "debug"
/// ```
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct LoggingConfig {
    #[serde(default = "default_log_level")]
    pub level: String,
    /// Write structured logs to this file in addition to stderr.
    pub file: Option<String>,
    /// Per-module log levels (tracing target syntax).
    #[serde(default)]
    pub targets: HashMap<String, String>,
}

fn default_log_level() -> String {
    "info".to_string()
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            file: None,
            targets: HashMap::new(),
        }
    }
}

/// Storage backend configuration.
///
/// Two backends are supported:
/// - `local` — file-based storage in a local directory (no S3/PostgreSQL needed)
/// - `s3` — S3 + PostgreSQL (production; implemented in Phase 5)
///
/// ```toml
/// [storage]
/// type = "local"
/// local_dir = "/var/lib/imgdelta"
///
/// # — or —
///
/// [storage]
/// type = "s3"
/// s3_bucket = "my-images"
/// database_url = "postgres://user:pass@localhost/imgdelta"
/// ```
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StorageConfig {
    Local {
        local_dir: PathBuf,
    },
    S3 {
        s3_bucket: String,
        s3_region: Option<String>,
        /// Override S3 endpoint URL (useful for MinIO / YC Object Storage).
        s3_endpoint: Option<String>,
        /// PostgreSQL DSN, e.g. `postgres://user:pass@localhost/imgdelta`.
        database_url: String,
    },
}

impl StorageConfig {
    /// Build the appropriate [`Storage`] implementation.
    pub async fn build(&self) -> anyhow::Result<Arc<dyn Storage>> {
        match self {
            StorageConfig::Local { local_dir } => {
                let s = LocalStorage::new(local_dir.clone())?;
                Ok(Arc::new(s))
            }
            StorageConfig::S3 { .. } => {
                let s = S3Storage::new(self).await?;
                Ok(Arc::new(s))
            }
        }
    }
}

/// Compressor behaviour configuration.
#[derive(Debug, Deserialize)]
pub struct CompressorConfig {
    /// Number of parallel worker threads. Default: number of logical CPUs.
    #[serde(default = "default_workers")]
    pub workers: usize,
    /// Fall back to passthrough if `delta_size >= source_size * threshold`.
    #[serde(default = "default_passthrough_threshold")]
    pub passthrough_threshold: f64,
    /// Encoder used when no routing rule matches.
    pub default_encoder: EncoderKind,
    /// Per-file-type routing rules evaluated in order.
    #[serde(default)]
    pub routing: Vec<RoutingRuleConfig>,
}

impl CompressorConfig {
    /// Build a [`RouterEncoder`] from this configuration.
    ///
    /// If no routing rules are configured, wraps the `default_encoder` in a
    /// [`RouterEncoder`] with no rules.  This guarantees the caller always
    /// receives a `RouterEncoder` regardless of config.
    pub fn build_router(&self) -> anyhow::Result<Arc<RouterEncoder>> {
        let fallback: Arc<dyn PatchEncoder> = make_encoder(&self.default_encoder);

        let mut rules: Vec<Box<dyn RoutingRule>> = Vec::new();
        for rule_cfg in &self.routing {
            let rule: Box<dyn RoutingRule> = match rule_cfg {
                RoutingRuleConfig::Glob { pattern, encoder } => {
                    Box::new(GlobRule::new(pattern, make_encoder(encoder))?)
                }
                RoutingRuleConfig::Elf { encoder } => Box::new(ElfRule::new(make_encoder(encoder))),
                RoutingRuleConfig::Size { max_bytes, encoder } => {
                    Box::new(SizeRule::new(*max_bytes, make_encoder(encoder)))
                }
                RoutingRuleConfig::Magic { hex, encoder } => {
                    let magic_bytes = hex::decode(hex)
                        .map_err(|e| anyhow::anyhow!("invalid magic hex '{}': {}", hex, e))?;
                    Box::new(MagicRule::new(magic_bytes, make_encoder(encoder)))
                }
            };
            rules.push(rule);
        }

        Ok(Arc::new(RouterEncoder::new(rules, fallback)))
    }
}

fn make_encoder(kind: &EncoderKind) -> Arc<dyn PatchEncoder> {
    match kind {
        EncoderKind::Xdelta3 => Arc::new(Xdelta3Encoder::new()),
        EncoderKind::TextDiff => Arc::new(TextDiffEncoder::new()),
        EncoderKind::Passthrough => Arc::new(PassthroughEncoder::new()),
    }
}

fn default_workers() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

fn default_passthrough_threshold() -> f64 {
    1.0
}

/// Root configuration structure.  Loaded from a TOML file.
///
/// Minimal example:
///
/// ```toml
/// [storage]
/// type = "local"
/// local_dir = "/var/lib/imgdelta"
///
/// [compressor]
/// workers = 8
/// default_encoder = "xdelta3"
///
/// [[compressor.routing]]
/// type = "glob"
/// pattern = "**/*.gz"
/// encoder = "passthrough"
/// ```
#[derive(Debug, Deserialize)]
pub struct Config {
    pub storage: StorageConfig,
    pub compressor: CompressorConfig,
    #[allow(dead_code)]
    #[serde(default)]
    pub logging: LoggingConfig,
}

impl Config {
    /// Load and parse a TOML configuration file.
    ///
    /// Returns an error if the file cannot be read or does not contain valid TOML
    /// matching the expected schema.
    pub fn from_file(path: &std::path::Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }
}
