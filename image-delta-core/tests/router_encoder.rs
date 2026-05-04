// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Integration tests for RouterEncoder glob-rule dispatch

/// Integration tests for [`RouterEncoder`] encode and decode routing.
///
/// Verifies that the router selects the correct encoder based on file
/// information during encoding, and the correct decoder based on
/// [`AlgorithmCode`] during decoding.
use std::sync::Arc;

use image_delta_core::{
    AlgorithmCode, Error, FilePatch, FileSnapshot, GlobRule, PassthroughEncoder, PatchEncoder,
    RouterEncoder, TextDiffEncoder, Xdelta3Encoder,
};

// ── helpers ───────────────────────────────────────────────────────────────────

fn snap<'a>(bytes: &'a [u8], path: &'a str) -> FileSnapshot<'a> {
    FileSnapshot {
        bytes,
        path,
        size: bytes.len() as u64,
        header: &bytes[..bytes.len().min(16)],
    }
}

fn snap_b<'a>(bytes: &'a [u8]) -> FileSnapshot<'a> {
    snap(bytes, "")
}

/// Build a router with:
/// - `**/*.txt` → TextDiffEncoder
/// - `**/*.zst` → PassthroughEncoder
/// - fallback    → Xdelta3Encoder
fn make_router() -> RouterEncoder {
    let text_enc: Arc<dyn PatchEncoder> = Arc::new(TextDiffEncoder::new());
    let pass_enc: Arc<dyn PatchEncoder> = Arc::new(PassthroughEncoder::new());
    let fallback: Arc<dyn PatchEncoder> = Arc::new(Xdelta3Encoder::new());

    let rules: Vec<Box<dyn image_delta_core::RoutingRule>> = vec![
        Box::new(GlobRule::new("**/*.txt", Arc::clone(&text_enc)).unwrap()),
        Box::new(GlobRule::new("**/*.zst", Arc::clone(&pass_enc)).unwrap()),
    ];
    RouterEncoder::new(rules, fallback)
}

// ── encode routing ────────────────────────────────────────────────────────────

/// A `.txt` file should be routed to TextDiffEncoder (produces TextDiff patch).
#[test]
fn router_encode_txt_file_uses_text_diff() {
    let router = make_router();
    let source = b"line one\nline two\n";
    let target = b"line one\nline two modified\n";

    let base = snap(source, "/etc/config.txt");
    let tgt = snap(target, "/etc/config.txt");

    let patch = router.encode(&base, &tgt).expect("encode must succeed");
    assert_eq!(
        patch.code,
        AlgorithmCode::TextDiff,
        "*.txt file should use TextDiff"
    );
}

/// A `.zst` file should be routed to PassthroughEncoder (produces Passthrough patch).
#[test]
fn router_encode_zst_file_uses_passthrough() {
    let router = make_router();
    let source = b"compressed data v1";
    let target = b"compressed data v2";

    let base = snap(source, "/var/cache/pkg.zst");
    let tgt = snap(target, "/var/cache/pkg.zst");

    let patch = router.encode(&base, &tgt).expect("encode must succeed");
    assert_eq!(
        patch.code,
        AlgorithmCode::Passthrough,
        "*.zst file should use Passthrough"
    );
}

/// An unrecognised path should fall back to Xdelta3Encoder.
#[test]
fn router_encode_unknown_path_falls_back_to_xdelta3() {
    let router = make_router();
    let source = b"\x7fELFsome binary";
    let target = b"\x7fELFother binary";

    let base = snap(source, "/usr/bin/app");
    let tgt = snap(target, "/usr/bin/app");

    let patch = router.encode(&base, &tgt).expect("encode must succeed");
    assert_eq!(
        patch.code,
        AlgorithmCode::Xdelta3,
        "unrecognised path should fall back to Xdelta3"
    );
}

/// `RouterEncoder::algorithm_code` must return `None` (no single algorithm).
#[test]
fn router_algorithm_code_is_none() {
    let router = make_router();
    assert_eq!(router.algorithm_code(), None);
}

// ── decode routing ────────────────────────────────────────────────────────────

/// A patch with code `Xdelta3` is decoded by the Xdelta3 decoder.
#[test]
fn router_decode_xdelta3_patch() {
    let router = make_router();

    // First encode with a concrete Xdelta3 encoder to get a valid patch.
    let source = b"\x7fELFbase content for xdelta test";
    let target = b"\x7fELFmodified content for xdelta test";
    let xdelta = Xdelta3Encoder::new();
    let patch = xdelta
        .encode(&snap_b(source), &snap_b(target))
        .expect("encode");
    assert_eq!(patch.code, AlgorithmCode::Xdelta3);

    // Router should decode it correctly.
    let recovered = router
        .decode(source, &patch)
        .expect("router decode must succeed");
    assert_eq!(recovered, target);
}

/// A patch with code `TextDiff` is decoded by the TextDiff decoder.
#[test]
fn router_decode_text_diff_patch() {
    let router = make_router();

    let source = b"original text\nline two\n";
    let target = b"modified text\nline two\n";
    let text = TextDiffEncoder::new();
    let patch = text
        .encode(&snap_b(source), &snap_b(target))
        .expect("encode");
    assert_eq!(patch.code, AlgorithmCode::TextDiff);

    let recovered = router
        .decode(source, &patch)
        .expect("router decode must succeed");
    assert_eq!(recovered, target);
}

/// A patch with code `Passthrough` is decoded by the Passthrough decoder.
#[test]
fn router_decode_passthrough_patch() {
    let router = make_router();

    let source = b"some bytes v1";
    let target = b"some bytes v2 with different content";
    let pass = PassthroughEncoder::new();
    let patch = pass
        .encode(&snap_b(source), &snap_b(target))
        .expect("encode");
    assert_eq!(patch.code, AlgorithmCode::Passthrough);

    let recovered = router
        .decode(source, &patch)
        .expect("router decode must succeed");
    assert_eq!(recovered, target);
}

/// A patch with an unknown Extended algorithm_id produces `Error::Decode`.
#[test]
fn router_decode_unknown_extended_algorithm_returns_error() {
    let router = make_router();

    let patch = FilePatch::extended(b"garbage".to_vec(), "unknown-algo-xyz");
    let err = router
        .decode(b"source", &patch)
        .expect_err("must fail for unknown extended algorithm");
    assert!(
        matches!(err, Error::Decode(_)),
        "expected Error::Decode, got {err:?}"
    );
}

// ── roundtrip through router ──────────────────────────────────────────────────

/// Encode via router, then decode via router — result must match original target.
#[test]
fn router_encode_decode_roundtrip_text_file() {
    let router = make_router();

    let source = b"key = old_value\nother = 1\n";
    let target = b"key = new_value\nother = 1\nextra = added\n";

    let base = snap(source, "/etc/app.txt");
    let tgt = snap(target, "/etc/app.txt");

    let patch = router.encode(&base, &tgt).expect("encode");
    let recovered = router.decode(source, &patch).expect("decode");
    assert_eq!(recovered, target);
}

/// Encode via router, then decode via router — binary file using fallback Xdelta3.
#[test]
fn router_encode_decode_roundtrip_binary_file() {
    let router = make_router();

    let source: Vec<u8> = (0u8..=127).collect();
    let mut target = source.clone();
    target[50] ^= 0xFF;

    let base = snap(&source, "/usr/lib/libfoo.so");
    let tgt = snap(&target, "/usr/lib/libfoo.so");

    let patch = router.encode(&base, &tgt).expect("encode");
    let recovered = router.decode(&source, &patch).expect("decode");
    assert_eq!(recovered, target.as_slice());
}
