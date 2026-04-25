// Config structs are used in Phase 5/6 when CLI commands are wired up.
#![allow(dead_code)]

use serde::Deserialize;
use std::collections::HashMap;

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

/// S3 + PostgreSQL storage configuration.
#[derive(Debug, Deserialize)]
pub struct StorageConfig {
    pub s3_bucket: String,
    pub s3_region: Option<String>,
    /// Override S3 endpoint URL (useful for MinIO / YC Object Storage).
    pub s3_endpoint: Option<String>,
    /// PostgreSQL DSN, e.g. `postgres://user:pass@localhost/imgdelta`.
    pub database_url: String,
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
/// s3_bucket = "my-images"
/// database_url = "postgres://user:pass@localhost/imgdelta"
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
    #[serde(default)]
    pub logging: LoggingConfig,
}

impl Config {
    /// Load and parse a TOML configuration file.
    pub fn from_file(path: &std::path::Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }
}
