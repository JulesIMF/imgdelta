// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Integration tests for the DirectoryImage format packing logic

/// Tests for [`Image::pack`] on [`DirectoryImage`].
use image_delta_core::{DirectoryImage, Image};
use std::fs;
use tempfile::tempdir;

// ── 1. Basic pack (copy) ──────────────────────────────────────────────────────

#[test]
fn test_directory_format_pack_copies_tree() {
    let source = tempdir().unwrap();
    let output = tempdir().unwrap();
    let out_path = output.path().join("packed");

    // Build a small tree inside source.
    fs::create_dir(source.path().join("sub")).unwrap();
    fs::write(source.path().join("top.txt"), b"top level").unwrap();
    fs::write(source.path().join("sub").join("nested.txt"), b"nested").unwrap();

    let fmt = DirectoryImage::new();
    fmt.pack(source.path(), &out_path)
        .expect("pack must succeed");

    assert!(
        out_path.join("top.txt").exists(),
        "top.txt must exist in output"
    );
    assert!(
        out_path.join("sub").join("nested.txt").exists(),
        "sub/nested.txt must exist in output"
    );
    assert_eq!(
        fs::read(out_path.join("top.txt")).unwrap(),
        b"top level",
        "top.txt content must match"
    );
    assert_eq!(
        fs::read(out_path.join("sub").join("nested.txt")).unwrap(),
        b"nested",
        "sub/nested.txt content must match"
    );
}

// ── 2. Pack replaces existing output ─────────────────────────────────────────

#[test]
fn test_directory_format_pack_replaces_existing_output() {
    let source = tempdir().unwrap();
    let output_dir = tempdir().unwrap();
    let out_path = output_dir.path().join("out");

    fs::create_dir(&out_path).unwrap();
    fs::write(out_path.join("stale.txt"), b"stale").unwrap();

    fs::write(source.path().join("fresh.txt"), b"fresh").unwrap();

    let fmt = DirectoryImage::new();
    fmt.pack(source.path(), &out_path)
        .expect("pack must succeed");

    assert!(
        !out_path.join("stale.txt").exists(),
        "stale file must be removed"
    );
    assert!(
        out_path.join("fresh.txt").exists(),
        "fresh file must be present"
    );
}

// ── 3. Pack empty source dir ─────────────────────────────────────────────────

#[test]
fn test_directory_format_pack_empty_source() {
    let source = tempdir().unwrap();
    let output = tempdir().unwrap();
    let out_path = output.path().join("empty_out");

    let fmt = DirectoryImage::new();
    fmt.pack(source.path(), &out_path)
        .expect("pack empty source must succeed");

    assert!(out_path.exists(), "output directory must be created");
    assert_eq!(
        fs::read_dir(&out_path).unwrap().count(),
        0,
        "output must be empty"
    );
}

// ── 4. Mount still works after pack ──────────────────────────────────────────

#[test]
fn test_directory_format_mount_after_pack() {
    let source = tempdir().unwrap();
    let output = tempdir().unwrap();
    let out_path = output.path().join("packed");

    fs::write(source.path().join("file.txt"), b"data").unwrap();

    let fmt = DirectoryImage::new();
    fmt.pack(source.path(), &out_path).unwrap();

    // Mounting the packed output must return the correct root.
    let handle = fmt.mount(&out_path).expect("mount must succeed");
    assert_eq!(handle.root(), out_path.as_path());
    assert!(
        handle.root().join("file.txt").exists(),
        "packed file must be accessible via mount handle"
    );
}
