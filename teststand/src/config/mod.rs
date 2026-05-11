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

use serde::Deserialize;
use std::path::PathBuf;

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
    /// imgdelta compressor defaults (workers, passthrough_threshold).
    #[serde(default)]
    pub compressor: CompressorDefaults,
}

#[derive(Debug, Deserialize, Clone)]
pub struct CompressorDefaults {
    #[serde(default = "default_workers")]
    #[allow(dead_code)]
    pub workers: usize,
    #[serde(default = "default_threshold")]
    pub passthrough_threshold: f64,
}

impl Default for CompressorDefaults {
    fn default() -> Self {
        Self {
            workers: default_workers(),
            passthrough_threshold: default_threshold(),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct TelegramConfig {
    pub bot_token: String,
    /// Telegram user / chat IDs to send notifications to.
    #[serde(default)]
    pub subscribers: Vec<i64>,
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
fn default_workers() -> usize {
    4
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
