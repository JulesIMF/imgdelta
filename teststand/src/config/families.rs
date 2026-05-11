// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Jules IMF
//
// image-delta — incremental disk-image compression toolkit
// FamilySpec and ImageSpec types loaded from TOML family config files.

use serde::{Deserialize, Serialize};

/// Collected families — built either from a single `families.toml`
/// (containing `[[family]]` sections) or from a directory of per-family files.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct FamiliesConfig {
    #[serde(rename = "family")]
    pub families: Vec<FamilySpec>,
}

/// One family — either the top-level of a per-family file or one
/// `[[family]]` entry in a multi-family file.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct FamilySpec {
    /// Unique family name, e.g. "debian-11".
    pub name: String,
    /// Human-readable label shown in the UI (optional).
    pub label: Option<String>,
    /// S3 base URL prefix (informational, not used at runtime).
    pub base_url: Option<String>,
    #[serde(rename = "image")]
    pub images: Vec<ImageSpec>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ImageSpec {
    /// Unique image ID, e.g. "centos-stream-8-v20220613".
    pub id: String,
    /// Full download URL for the qcow2 file.
    pub url: String,
    /// Uncompressed storage size in bytes (storage_size from YC metadata).
    pub size_bytes: Option<u64>,
    /// Expected SHA-256 hex digest of the downloaded file (optional).
    pub sha256: Option<String>,
    /// Image format: "qcow2" | "directory" (default: "qcow2").
    #[serde(default = "default_format")]
    pub format: String,
}

fn default_format() -> String {
    "qcow2".into()
}

/// Parse a single per-family TOML file (top-level fields = FamilySpec).
pub fn load_family_file(path: &std::path::Path) -> crate::error::Result<FamilySpec> {
    let text = std::fs::read_to_string(path)?;
    let spec: FamilySpec = toml::from_str(&text)?;
    Ok(spec)
}
