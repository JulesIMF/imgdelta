// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Jules IMF
//
// image-delta — incremental disk-image compression toolkit
// config_test.rs — smoke-tests for configuration parsing.

//! Configuration parsing smoke-tests.

use teststand::config::families::FamilySpec;
use teststand::config::{load_experiment, load_families, TeststandConfig};

#[test]
fn parse_minimal_config() {
    let toml = r#"
workdir    = "/tmp/ts"
auth_token = "secret"
"#;
    let cfg: TeststandConfig = toml::from_str(toml).expect("parse failed");
    assert_eq!(cfg.port, 8080);
    assert!(cfg.telegram.is_none());
}

#[test]
fn parse_telegram_config() {
    let toml = r#"
workdir    = "/tmp/ts"
auth_token = "secret"
[telegram]
bot_token   = "123:ABC"
subscribers = [100, 200]
"#;
    let cfg: TeststandConfig = toml::from_str(toml).expect("parse failed");
    let tg = cfg.telegram.expect("telegram missing");
    assert_eq!(tg.bot_token, "123:ABC");
    assert_eq!(tg.subscribers, vec![100i64, 200i64]);
}

#[test]
fn parse_chain_experiment() {
    let toml = r#"
name           = "test-chain"
family         = "ubuntu-2204"
workers        = [1, 2, 4]
runs_per_pair  = 2
"#;
    let spec = load_experiment(toml).expect("parse failed");
    assert_eq!(spec.name, "test-chain");
    assert_eq!(spec.family, "ubuntu-2204");
    assert_eq!(spec.workers, vec![1usize, 2, 4]);
    assert_eq!(spec.runs_per_pair, 2);
}

#[test]
fn parse_images_filter() {
    let toml = r#"
name          = "scale-test"
family        = "ubuntu-2204"
workers       = [1, 2, 4, 8]
runs_per_pair = 3
images        = ["ubuntu-v1", "ubuntu-v2", "ubuntu-v3"]
"#;
    let spec = load_experiment(toml).expect("parse failed");
    assert_eq!(spec.workers.len(), 4);
    let imgs = spec.images.as_deref().unwrap_or(&[]);
    assert_eq!(imgs.len(), 3);
    assert_eq!(imgs[0], "ubuntu-v1");
    assert_eq!(imgs[1], "ubuntu-v2");
}

#[test]
fn parse_per_family_toml() {
    let toml = r#"
name     = "centos-stream-8"
label    = "CentOS Stream 8"
base_url = "https://storage.example.com/centos-stream-8"

[[image]]
id         = "centos-stream-8-v20220613"
url        = "https://storage.example.com/centos-stream-8/centos-stream-8-v20220613.qcow2"
size_bytes = 2868903936
format     = "qcow2"

[[image]]
id         = "centos-stream-8-v20220620"
url        = "https://storage.example.com/centos-stream-8/centos-stream-8-v20220620.qcow2"
size_bytes = 2864709632
format     = "qcow2"
"#;
    let spec: FamilySpec = toml::from_str(toml).expect("parse failed");
    assert_eq!(spec.name, "centos-stream-8");
    assert_eq!(spec.label.as_deref(), Some("CentOS Stream 8"));
    assert_eq!(spec.images.len(), 2);
    assert_eq!(spec.images[0].id, "centos-stream-8-v20220613");
    assert_eq!(spec.images[0].format, "qcow2");
    assert_eq!(spec.images[0].size_bytes, Some(2868903936));
}

#[test]
fn load_families_from_directory() {
    // Point load_families at the real families/ directory in the crate
    let families_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("families");
    if !families_dir.exists() {
        // Skip if not present (CI without the directory)
        return;
    }
    let cfg = load_families(&families_dir).expect("load_families failed");
    // Should have loaded all 5 families
    assert_eq!(
        cfg.families.len(),
        5,
        "expected 5 family files, got {}",
        cfg.families.len()
    );
    // Each family should have 25 images
    for fam in &cfg.families {
        assert!(!fam.images.is_empty(), "family {} has no images", fam.name);
        assert_eq!(
            fam.images.len(),
            25,
            "family {} should have 25 images",
            fam.name
        );
    }
}

#[test]
fn load_families_from_single_file() {
    let toml = r#"
[[family]]
name = "test-family"

[[family.image]]
id     = "img-1"
url    = "https://example.com/img-1.qcow2"
format = "qcow2"
"#;
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), toml).unwrap();
    let cfg = load_families(tmp.path()).expect("load_families failed");
    assert_eq!(cfg.families.len(), 1);
    assert_eq!(cfg.families[0].images.len(), 1);
}
