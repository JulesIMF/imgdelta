// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Jules IMF
//
// image-delta — incremental disk-image compression toolkit
// ExperimentSpec TOML definition for chain and scalability benchmarks.

use serde::{Deserialize, Serialize};

/// Experiment TOML submitted via the web UI or CLI.
///
/// Example (Chain):
/// ```toml
/// name = "debian-12-chain"
/// family = "debian-12"
/// kind = "Chain"
/// runs_per_pair = 1
/// workers = [16]
/// ```
///
/// Example (Scalability):
/// ```toml
/// name = "debian-12-scalability"
/// family = "debian-12"
/// kind = "Scalability"
/// base_image_id = "debian-12-v20260101"
/// target_image_id = "debian-12-v20260201"
/// workers = [1, 2, 3, 4, 6, 8, 12, 16]
/// runs_per_pair = 3
/// ```
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ExperimentSpec {
    /// Unique human-readable name for this experiment run.
    pub name: String,
    /// Family name as defined in families.toml.
    pub family: String,
    /// Experiment type.
    pub kind: ExperimentKind,
    /// Worker counts to test.  Each count produces a separate set of runs.
    #[serde(default = "default_workers")]
    pub workers: Vec<usize>,
    /// How many times to repeat compress+decompress for each (pair, workers) combo.
    #[serde(default = "default_runs")]
    pub runs_per_pair: usize,
    /// For Scalability: the base image ID within the family.
    pub base_image_id: Option<String>,
    /// For Scalability: the target image ID within the family.
    pub target_image_id: Option<String>,
    /// Override passthrough threshold (default from teststand.toml).
    pub passthrough_threshold: Option<f64>,
    /// For Chain: restrict to these image IDs from the family (in order).
    /// If omitted, all images in the family are used.
    /// Example: `images = ["centos-stream-8-v20220613", "centos-stream-8-v20220620"]`
    pub images: Option<Vec<String>>,
    /// If true, downloaded qcow2 images are NOT deleted after the experiment.
    /// Default: false — images are evicted to save disk space.
    pub keep_images: Option<bool>,
    /// Extra notes stored with results.
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub enum ExperimentKind {
    /// Compress all consecutive pairs in the family chain.
    /// Produces C*(n) curve data.
    Chain,
    /// Compress a single pair at multiple worker counts.
    /// Produces scalability / speedup data.
    Scalability,
}

fn default_workers() -> Vec<usize> {
    vec![4]
}
fn default_runs() -> usize {
    1
}

impl ExperimentKind {
    pub fn as_str(&self) -> &str {
        match self {
            ExperimentKind::Chain => "Chain",
            ExperimentKind::Scalability => "Scalability",
        }
    }
}
