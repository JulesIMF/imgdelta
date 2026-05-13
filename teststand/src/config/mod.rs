// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Jules IMF
//
// image-delta — incremental disk-image compression toolkit
// Top-level teststand configuration: ServerConfig, CompressorConfig.

pub mod experiment;
pub mod families;

#[allow(unused_imports)]
pub use experiment::ExperimentSpec;
#[allow(unused_imports)]
pub use families::{load_family_file, FamiliesConfig, FamilySpec, ImageSpec};

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use image_delta_core::encoding::{
    PassthroughEncoder, PatchEncoder, TextDiffEncoder, Xdelta3Encoder,
};
use image_delta_core::{ElfRule, GlobRule, MagicRule, RouterEncoder, RoutingRule, SizeRule};

// ── Encoder / routing config (mirrors cli/src/config.rs, no cli crate dep) ──

/// Which delta encoder to use.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EncoderKind {
    #[default]
    Xdelta3,
    TextDiff,
    Passthrough,
}

/// One entry in the `[[compressor.routing]]` array.
#[derive(Debug, Clone, Deserialize, Serialize)]
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
        hex: String,
        encoder: EncoderKind,
    },
}

/// Compressor configuration embedded in teststand.toml or an ExperimentSpec.
///
/// Example (in teststand.toml):
/// ```toml
/// [compressor]
/// passthrough_threshold = 1.0
/// default_encoder = "xdelta3"
///
/// [[compressor.routing]]
/// type = "glob"
/// pattern = "**/*.gz"
/// encoder = "passthrough"
/// ```
///
/// Note: `workers` is NOT part of compressor config in teststand — it comes
/// from the experiment spec `workers = [1, 2, 4, 8]` and is the variable
/// being benchmarked.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CompressorConfig {
    /// Fall back to passthrough if `delta_size >= source_size * threshold`.
    #[serde(default = "default_threshold")]
    pub passthrough_threshold: f64,
    /// Encoder used when no routing rule matches.
    #[serde(default)]
    pub default_encoder: EncoderKind,
    /// Per-file-type routing rules evaluated in order.
    #[serde(default)]
    pub routing: Vec<RoutingRuleConfig>,
}

impl Default for CompressorConfig {
    fn default() -> Self {
        Self {
            passthrough_threshold: default_threshold(),
            default_encoder: EncoderKind::Xdelta3,
            routing: Vec::new(),
        }
    }
}

impl CompressorConfig {
    /// Build an `Arc<RouterEncoder>` from this configuration.
    ///
    /// Emits a `tracing::info!` line listing all routing rules so misconfigured
    /// or inactive rules are visible in logs.
    pub fn build_router(&self) -> crate::error::Result<Arc<RouterEncoder>> {
        // Dump routing rules to the log so operators can verify configuration.
        tracing::info!(
            default_encoder = ?self.default_encoder,
            passthrough_threshold = self.passthrough_threshold,
            routing_rules = self.routing.len(),
            "building router",
        );
        for (i, rule) in self.routing.iter().enumerate() {
            let desc = match rule {
                RoutingRuleConfig::Glob { pattern, encoder } => {
                    format!("[{i}] glob({pattern:?}) → {encoder:?}")
                }
                RoutingRuleConfig::Elf { encoder } => format!("[{i}] elf → {encoder:?}"),
                RoutingRuleConfig::Size { max_bytes, encoder } => {
                    format!("[{i}] size(<={max_bytes}) → {encoder:?}")
                }
                RoutingRuleConfig::Magic { hex, encoder } => {
                    format!("[{i}] magic(0x{hex}) → {encoder:?}")
                }
            };
            tracing::info!("{desc}");
        }
        let fallback: Arc<dyn PatchEncoder> = make_encoder(&self.default_encoder);
        let mut rules: Vec<Box<dyn RoutingRule>> = Vec::new();
        for rule_cfg in &self.routing {
            let rule: Box<dyn RoutingRule> = match rule_cfg {
                RoutingRuleConfig::Glob { pattern, encoder } => Box::new(
                    GlobRule::new(pattern, make_encoder(encoder))
                        .map_err(|e| crate::error::Error::Config(e.to_string()))?,
                ),
                RoutingRuleConfig::Elf { encoder } => Box::new(ElfRule::new(make_encoder(encoder))),
                RoutingRuleConfig::Size { max_bytes, encoder } => {
                    Box::new(SizeRule::new(*max_bytes, make_encoder(encoder)))
                }
                RoutingRuleConfig::Magic { hex, encoder } => {
                    let bytes = hex::decode(hex).map_err(|e| {
                        crate::error::Error::Config(format!("invalid magic hex '{hex}': {e}"))
                    })?;
                    Box::new(MagicRule::new(bytes, make_encoder(encoder)))
                }
            };
            rules.push(rule);
        }
        let mut router = RouterEncoder::new(rules, fallback);
        router.set_passthrough_threshold(self.passthrough_threshold);
        Ok(Arc::new(router))
    }
}

fn make_encoder(kind: &EncoderKind) -> Arc<dyn PatchEncoder> {
    match kind {
        EncoderKind::Xdelta3 => Arc::new(Xdelta3Encoder::new()),
        EncoderKind::TextDiff => Arc::new(TextDiffEncoder::new()),
        EncoderKind::Passthrough => Arc::new(PassthroughEncoder::new()),
    }
}

// ── Top-level config ──────────────────────────────────────────────────────────

/// Top-level teststand configuration (teststand.toml).
#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct TeststandConfig {
    /// Directory where images are downloaded, storage lives, and results are written.
    pub workdir: PathBuf,
    /// TCP port for the web interface.
    #[serde(default = "default_port")]
    pub port: u16,
    /// Static auth token.  Store this in a password manager and paste it into
    /// the web UI on first visit — it will be saved to localStorage.
    pub auth_token: String,
    /// Number of images to keep pre-downloaded ahead of the current experiment.
    #[serde(default = "default_prefetch")]
    pub prefetch_ahead: usize,
    /// Optional Telegram configuration.
    pub telegram: Option<TelegramConfig>,
    /// Default compressor settings; overridable per experiment.
    #[serde(default)]
    pub compressor: CompressorConfig,
    /// Unix nice value to set at startup (0–19).  Recommended: 10.
    /// If the call fails (e.g. process lacks permission to lower niceness),
    /// a warning is logged and the process continues with its current priority.
    pub nice: Option<i32>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct TelegramConfig {
    /// Set to false to disable all Telegram notifications and bot commands
    /// without removing the credentials from the config file.
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub bot_token: String,
    /// Telegram user / chat IDs to send notifications to.
    #[serde(default)]
    pub subscribers: Vec<i64>,
}

fn default_true() -> bool {
    true
}

impl TeststandConfig {
    /// Root directory for a specific experiment (experiments/{id}/).
    pub fn experiment_dir(&self, experiment_id: &str) -> std::path::PathBuf {
        self.workdir.join("experiments").join(experiment_id)
    }
    /// Isolated LocalStorage for one experiment (experiments/{id}/storage/).
    pub fn experiment_storage_dir(&self, experiment_id: &str) -> std::path::PathBuf {
        self.experiment_dir(experiment_id).join("storage")
    }
    /// Working directories for an experiment (experiments/{id}/workdirs/).
    #[allow(dead_code)]
    pub fn experiment_workdirs(&self, experiment_id: &str) -> std::path::PathBuf {
        self.experiment_dir(experiment_id).join("workdirs")
    }
    /// Shared image download cache (not per-experiment).
    pub fn images_dir(&self) -> std::path::PathBuf {
        self.workdir.join("images")
    }
    pub fn db_path(&self) -> std::path::PathBuf {
        self.workdir.join("teststand.db")
    }
}

fn default_port() -> u16 {
    8080
}
fn default_prefetch() -> usize {
    2
}
fn default_threshold() -> f64 {
    1.0
}

pub fn load_config(path: &std::path::Path) -> crate::error::Result<TeststandConfig> {
    let text = std::fs::read_to_string(path)?;
    let cfg: TeststandConfig = toml::from_str(&text)?;
    Ok(cfg)
}

/// Load families from either:
/// - a directory: reads every `*.toml` file inside as a `FamilySpec`
/// - a single file: reads it as a `FamiliesConfig` with `[[family]]` sections
pub fn load_families(path: &std::path::Path) -> crate::error::Result<FamiliesConfig> {
    if path.is_dir() {
        let mut families = Vec::new();
        let mut entries: Vec<_> = std::fs::read_dir(path)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|x| x == "toml").unwrap_or(false))
            .collect();
        entries.sort_by_key(|e| e.path());
        for entry in entries {
            match load_family_file(&entry.path()) {
                Ok(spec) => families.push(spec),
                Err(e) => {
                    tracing::warn!(path = %entry.path().display(), err = %e, "skipping family file")
                }
            }
        }
        Ok(FamiliesConfig { families })
    } else {
        let text = std::fs::read_to_string(path)?;
        let cfg: FamiliesConfig = toml::from_str(&text)?;
        Ok(cfg)
    }
}

pub fn load_experiment(text: &str) -> crate::error::Result<ExperimentSpec> {
    let spec: ExperimentSpec = toml::from_str(text)?;
    Ok(spec)
}

// Silence unused import warning for HashMap (used by LoggingConfig in cli but not here).
#[allow(dead_code)]
type _HashMap = HashMap<String, String>;
