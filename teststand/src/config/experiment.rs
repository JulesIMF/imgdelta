// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Jules IMF
//
// image-delta — incremental disk-image compression toolkit
// ExperimentSpec TOML definition for base-relative compression benchmarks.

use serde::{Deserialize, Serialize};

use crate::config::CompressorConfig;

/// Experiment TOML submitted via the web UI or CLI.
///
/// Each experiment compresses every selected image against `images[0]` (the
/// earliest / base image).  Worker counts and run repetitions are configurable.
///
/// Example:
/// ```toml
/// name = "centos-stream-8-baseline"
/// family = "centos-stream-8"
/// workers = [1, 2, 4, 8]
/// runs_per_pair = 3
/// # leave images empty to use all images in the family
/// images = ["centos-stream-8-v20220613", "centos-stream-8-v20220620"]
///
/// # Optional: override encoder from teststand.toml default.
/// [compressor]
/// default_encoder = "xdelta3"
/// passthrough_threshold = 1.0
///
/// [[compressor.routing]]
/// type = "glob"
/// pattern = "**/*.gz"
/// encoder = "passthrough"
/// ```
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ExperimentSpec {
    /// Unique human-readable name for this experiment run.
    pub name: String,
    /// Family name as defined in families.toml.
    pub family: String,
    /// Worker counts to test.  Each count produces a separate set of runs.
    #[serde(default = "default_workers")]
    pub workers: Vec<usize>,
    /// How many times to repeat compress for each (target, workers) combo.
    #[serde(default = "default_runs")]
    pub runs_per_pair: usize,
    /// Compressor configuration for this experiment.
    /// If omitted, the teststand.toml `[compressor]` defaults are used.
    pub compressor: Option<CompressorConfig>,
    /// Restrict to these image IDs from the family (in declaration order).
    /// images[0] is used as the base; images[1..] are the targets.
    /// If omitted, all images in the family are used.
    pub images: Option<Vec<String>>,
    /// If true, downloaded qcow2 images are NOT deleted after the experiment.
    /// Default: false — images are evicted to save disk space.
    pub keep_images: Option<bool>,
    /// Extra notes stored with results.
    pub notes: Option<String>,
}

fn default_workers() -> Vec<usize> {
    vec![4]
}
fn default_runs() -> usize {
    1
}
