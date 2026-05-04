//! Integration tests for `DefaultCompressor::compress()` (Phase 6.D).
//!
//! Tests use `DirectoryImage` (no mounting needed) and `FakeStorage` (in-memory).
//! They verify the full compress pipeline from the orchestrator's perspective:
//! manifest structure, status lifecycle, stats computation, and manifest
//! round-trip serialisation.

mod common;

use std::sync::Arc;

use common::FakeStorage;
use filetime::FileTime;
use image_delta_core::manifest::{Data, Manifest, PartitionContent, Patch};
use image_delta_core::{
    CompressOptions, Compressor, DefaultCompressor, DirectoryImage, Storage, Xdelta3Encoder,
};
use tempfile::TempDir;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn write(dir: &std::path::Path, rel: &str, content: &[u8]) {
    let full = dir.join(rel);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(full, content).unwrap();
}

/// Copy the mtime from `src` to `dst` so the walkdir stage considers the file
/// unchanged (mtime tolerance is ±1 s).
fn copy_mtime(src: &std::path::Path, dst: &std::path::Path) {
    let mtime = std::fs::metadata(src).unwrap().modified().unwrap();
    filetime::set_file_mtime(dst, FileTime::from_system_time(mtime)).unwrap();
}

fn make_compressor(storage: &FakeStorage) -> impl Compressor {
    DefaultCompressor::with_encoder(
        Arc::new(DirectoryImage::new()),
        Arc::new(storage.clone()),
        Arc::new(Xdelta3Encoder::new()),
    )
}

fn base_options(image_id: &str, base_image_id: Option<&str>) -> CompressOptions {
    CompressOptions {
        image_id: image_id.into(),
        base_image_id: base_image_id.map(str::to_string),
        workers: 1,
        passthrough_threshold: 1.0,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Status lifecycle
// ─────────────────────────────────────────────────────────────────────────────

/// After a successful `compress()` call the image status must be `"compressed"`.
#[tokio::test]
async fn test_compress_status_lifecycle() {
    let storage = FakeStorage::new();
    let base_dir = TempDir::new().unwrap();
    let target_dir = TempDir::new().unwrap();

    write(target_dir.path(), "etc/motd", b"hello\n");

    let compressor = make_compressor(&storage);
    compressor
        .compress(
            base_dir.path(),
            target_dir.path(),
            base_options("img-status", None),
        )
        .await
        .unwrap();

    assert_eq!(
        storage.image_status("img-status").as_deref(),
        Some("compressed"),
        "image status must be 'compressed' after successful compress()"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Manifest structure
// ─────────────────────────────────────────────────────────────────────────────

/// Golden test: compress two directory versions and verify the uploaded manifest
/// has the correct structure (header fields, partition count, record types).
#[tokio::test]
async fn test_compress_golden_manifest_structure() {
    let storage = FakeStorage::new();
    let base_dir = TempDir::new().unwrap();
    let target_dir = TempDir::new().unwrap();

    let passwd_v1 = b"root:x:0:0:root:/root:/bin/bash\n";
    let passwd_v2 = b"root:x:0:0:root:/root:/bin/bash\nuser:x:1000::\n";
    let shadow = b"root:!:19000:::::::\n";

    write(base_dir.path(), "etc/passwd", passwd_v1);
    write(base_dir.path(), "etc/shadow", shadow);
    write(base_dir.path(), "lib/libz.so.1", b"old libz");

    write(target_dir.path(), "etc/passwd", passwd_v2); // changed
    write(target_dir.path(), "etc/shadow", shadow); // unchanged (copy mtime)
    write(target_dir.path(), "usr/bin/newutil", b"brand new tool"); // added
                                                                    // lib/libz.so.1 removed from target

    copy_mtime(
        &base_dir.path().join("etc/shadow"),
        &target_dir.path().join("etc/shadow"),
    );

    let compressor = make_compressor(&storage);
    let stats = compressor
        .compress(
            base_dir.path(),
            target_dir.path(),
            base_options("img-gold", Some("img-base")),
        )
        .await
        .unwrap();

    // ── Stats ─────────────────────────────────────────────────────────────────
    assert!(stats.files_patched >= 1, "etc/passwd was changed");
    assert!(stats.files_added >= 1, "usr/bin/newutil was added");
    assert!(stats.files_removed >= 1, "lib/libz.so.1 was removed");
    assert!(stats.elapsed_secs >= 0.0);

    // ── Download and parse manifest ───────────────────────────────────────────
    let manifest_bytes = storage.download_manifest("img-gold").await.unwrap();
    let manifest = Manifest::from_bytes(&manifest_bytes).unwrap();

    assert_eq!(manifest.header.version, image_delta_core::MANIFEST_VERSION);
    assert_eq!(manifest.header.image_id, "img-gold");
    assert_eq!(manifest.header.base_image_id, Some("img-base".into()));
    assert_eq!(manifest.header.format, "directory");
    assert_eq!(manifest.partitions.len(), 1);

    let PartitionContent::Fs { fs_type, records } = &manifest.partitions[0].content else {
        panic!("expected PartitionContent::Fs");
    };
    assert_eq!(fs_type, "directory");

    // etc/passwd: changed → Patch::Real
    let passwd = records
        .iter()
        .find(|r| r.new_path.as_deref() == Some("etc/passwd"))
        .expect("etc/passwd must appear in manifest");
    assert!(
        matches!(passwd.patch, Some(Patch::Real(_))),
        "etc/passwd must have Patch::Real"
    );

    // etc/shadow: unchanged → must NOT appear
    assert!(
        !records
            .iter()
            .any(|r| r.new_path.as_deref() == Some("etc/shadow")
                || r.old_path.as_deref() == Some("etc/shadow")),
        "unchanged etc/shadow must not appear in manifest"
    );

    // usr/bin/newutil: added → Data::BlobRef
    let newutil = records
        .iter()
        .find(|r| r.new_path.as_deref() == Some("usr/bin/newutil"))
        .expect("usr/bin/newutil must appear in manifest");
    assert!(
        matches!(newutil.data, Some(Data::BlobRef(_))),
        "added file must have Data::BlobRef"
    );

    // lib/libz.so.1: removed → deletion record
    assert!(
        records
            .iter()
            .any(|r| r.old_path.as_deref() == Some("lib/libz.so.1") && r.new_path.is_none()),
        "lib/libz.so.1 must have a deletion record"
    );

    // No Lazy patches must remain in the finalised manifest.
    for r in records {
        assert!(
            !matches!(r.patch, Some(Patch::Lazy { .. })),
            "Lazy patch must not appear in finalised manifest: {r:?}"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// First compression (no base image)
// ─────────────────────────────────────────────────────────────────────────────

/// When `base_image_id = None`, all files are new.  Every file record must have
/// `Data::BlobRef` and `patch = None`.  The manifest header must have
/// `base_image_id = None`.
#[tokio::test]
async fn test_compress_first_compression_no_base() {
    let storage = FakeStorage::new();
    let base_dir = TempDir::new().unwrap(); // intentionally empty
    let target_dir = TempDir::new().unwrap();

    let file_count: u32 = 8;
    for i in 0..file_count {
        write(
            target_dir.path(),
            &format!("usr/lib/lib{i:02}.so"),
            format!("library {i} content").as_bytes(),
        );
    }

    let compressor = make_compressor(&storage);
    let stats = compressor
        .compress(
            base_dir.path(),
            target_dir.path(),
            base_options("img-first", None),
        )
        .await
        .unwrap();

    assert_eq!(stats.files_added, file_count as usize);
    assert_eq!(stats.files_patched, 0);
    assert_eq!(stats.files_removed, 0);

    let manifest_bytes = storage.download_manifest("img-first").await.unwrap();
    let manifest = Manifest::from_bytes(&manifest_bytes).unwrap();

    assert!(
        manifest.header.base_image_id.is_none(),
        "no-base manifest must have base_image_id = None"
    );

    let PartitionContent::Fs { records, .. } = &manifest.partitions[0].content else {
        panic!("expected PartitionContent::Fs")
    };

    let file_records: Vec<_> = records
        .iter()
        .filter(|r| matches!(r.entry_type, image_delta_core::manifest::EntryType::File))
        .collect();

    assert_eq!(
        file_records.len(),
        file_count as usize,
        "all {file_count} files must appear in manifest"
    );

    for r in &file_records {
        assert!(
            matches!(r.data, Some(Data::BlobRef(_))),
            "first-compression file must have Data::BlobRef: {:?}",
            r.data
        );
        assert!(
            r.patch.is_none(),
            "first-compression file must have no patch"
        );
    }

    assert_eq!(
        storage.uploaded_blob_count(),
        file_count as usize,
        "each unique file must produce one blob"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Manifest round-trip serialisation
// ─────────────────────────────────────────────────────────────────────────────

/// The manifest uploaded to storage must survive a MessagePack round-trip with
/// no field loss.
#[tokio::test]
async fn test_compress_manifest_roundtrip() {
    let storage = FakeStorage::new();
    let base_dir = TempDir::new().unwrap();
    let target_dir = TempDir::new().unwrap();

    write(base_dir.path(), "bin/tool", b"tool v1.0 content");
    write(target_dir.path(), "bin/tool", b"tool v2.0 content");

    let compressor = make_compressor(&storage);
    compressor
        .compress(
            base_dir.path(),
            target_dir.path(),
            base_options("img-rt", Some("img-base")),
        )
        .await
        .unwrap();

    let raw = storage.download_manifest("img-rt").await.unwrap();
    let recovered: Manifest = rmp_serde::from_slice(&raw).unwrap();

    assert_eq!(recovered.header.image_id, "img-rt");
    assert_eq!(recovered.header.base_image_id, Some("img-base".into()));
    assert_eq!(recovered.header.format, "directory");
    assert_eq!(recovered.header.version, image_delta_core::MANIFEST_VERSION);
    assert_eq!(recovered.partitions.len(), 1);

    let PartitionContent::Fs { records, .. } = &recovered.partitions[0].content else {
        panic!("expected PartitionContent::Fs after round-trip");
    };

    // The changed file must have a Real patch.
    let tool = records
        .iter()
        .find(|r| r.new_path.as_deref() == Some("bin/tool"))
        .expect("bin/tool must appear after round-trip");
    assert!(
        matches!(tool.patch, Some(Patch::Real(_))),
        "bin/tool must have Patch::Real after round-trip"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Sequential compression chain (v0 → v1 → v2)
// ─────────────────────────────────────────────────────────────────────────────

/// Compress three successive image versions.  Each manifest's `base_image_id`
/// must point to the correct predecessor, forming a valid delta chain.
#[tokio::test]
async fn test_compress_sequential_chain() {
    let storage = FakeStorage::new();

    let v0_dir = TempDir::new().unwrap();
    let v1_dir = TempDir::new().unwrap();
    let v2_dir = TempDir::new().unwrap();

    // v0: three files
    write(v0_dir.path(), "etc/hosts", b"127.0.0.1 localhost\n");
    write(v0_dir.path(), "etc/resolv.conf", b"nameserver 8.8.8.8\n");
    write(v0_dir.path(), "usr/bin/app", b"app version 1.0\n");

    // v1: change one file, add one
    write(v1_dir.path(), "etc/hosts", b"127.0.0.1 localhost\n"); // same
    write(v1_dir.path(), "etc/resolv.conf", b"nameserver 1.1.1.1\n"); // changed
    write(v1_dir.path(), "usr/bin/app", b"app version 1.0\n"); // same
    write(v1_dir.path(), "usr/bin/newcmd", b"brand new command\n"); // added

    copy_mtime(
        &v0_dir.path().join("etc/hosts"),
        &v1_dir.path().join("etc/hosts"),
    );
    copy_mtime(
        &v0_dir.path().join("usr/bin/app"),
        &v1_dir.path().join("usr/bin/app"),
    );

    // v2: change another file
    write(
        v2_dir.path(),
        "etc/hosts",
        b"127.0.0.1 localhost\n192.168.1.1 server\n",
    ); // changed
    write(v2_dir.path(), "etc/resolv.conf", b"nameserver 1.1.1.1\n"); // same as v1
    write(v2_dir.path(), "usr/bin/app", b"app version 2.0\n"); // changed
    write(v2_dir.path(), "usr/bin/newcmd", b"brand new command\n"); // same as v1

    copy_mtime(
        &v1_dir.path().join("etc/resolv.conf"),
        &v2_dir.path().join("etc/resolv.conf"),
    );
    copy_mtime(
        &v1_dir.path().join("usr/bin/newcmd"),
        &v2_dir.path().join("usr/bin/newcmd"),
    );

    let compressor = make_compressor(&storage);

    // Compress v0 (no base).
    compressor
        .compress(
            v0_dir.path(), // source_root ignored when base_image_id=None
            v0_dir.path(),
            base_options("img-v0", None),
        )
        .await
        .unwrap();

    // Compress v1 on top of v0.
    compressor
        .compress(
            v0_dir.path(),
            v1_dir.path(),
            base_options("img-v1", Some("img-v0")),
        )
        .await
        .unwrap();

    // Compress v2 on top of v1.
    compressor
        .compress(
            v1_dir.path(),
            v2_dir.path(),
            base_options("img-v2", Some("img-v1")),
        )
        .await
        .unwrap();

    // Verify chain: each manifest points to the right predecessor.
    for (id, expected_base) in [
        ("img-v0", None),
        ("img-v1", Some("img-v0")),
        ("img-v2", Some("img-v1")),
    ] {
        let raw = storage.download_manifest(id).await.unwrap();
        let m = Manifest::from_bytes(&raw).unwrap();
        assert_eq!(
            m.header.base_image_id.as_deref(),
            expected_base,
            "{id}: base_image_id mismatch"
        );
        assert_eq!(
            storage.image_status(id).as_deref(),
            Some("compressed"),
            "{id}: status must be 'compressed'"
        );
    }

    // v1 manifest must show resolv.conf as changed and hosts as unchanged.
    let v1_raw = storage.download_manifest("img-v1").await.unwrap();
    let v1_m = Manifest::from_bytes(&v1_raw).unwrap();
    let PartitionContent::Fs {
        records: v1_records,
        ..
    } = &v1_m.partitions[0].content
    else {
        panic!()
    };
    assert!(
        v1_records
            .iter()
            .any(|r| r.new_path.as_deref() == Some("etc/resolv.conf") && r.patch.is_some()),
        "v1: etc/resolv.conf must be changed"
    );
    assert!(
        !v1_records
            .iter()
            .any(|r| r.new_path.as_deref() == Some("etc/hosts")
                && r.old_path.as_deref() == Some("etc/hosts")),
        "v1: etc/hosts unchanged — must not appear as modified"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Idempotency: compress the same target twice
// ─────────────────────────────────────────────────────────────────────────────

/// Compressing the same target twice with different `image_id`s must produce
/// identical manifest records (same record count, same paths, same patch SHAs).
/// This tests that the pipeline is deterministic.
#[tokio::test]
async fn test_compress_deterministic() {
    let storage = FakeStorage::new();
    let base_dir = TempDir::new().unwrap();
    let target_dir = TempDir::new().unwrap();

    write(base_dir.path(), "etc/config", b"key=old_value\n");
    write(target_dir.path(), "etc/config", b"key=new_value\n");
    write(target_dir.path(), "etc/new_file", b"brand new\n");

    let compressor = make_compressor(&storage);

    // First run.
    compressor
        .compress(
            base_dir.path(),
            target_dir.path(),
            base_options("img-det-1", Some("img-base")),
        )
        .await
        .unwrap();

    // Second run (different image_id, same content).
    compressor
        .compress(
            base_dir.path(),
            target_dir.path(),
            base_options("img-det-2", Some("img-base")),
        )
        .await
        .unwrap();

    let m1 = Manifest::from_bytes(&storage.download_manifest("img-det-1").await.unwrap()).unwrap();
    let m2 = Manifest::from_bytes(&storage.download_manifest("img-det-2").await.unwrap()).unwrap();

    let PartitionContent::Fs {
        records: records1, ..
    } = &m1.partitions[0].content
    else {
        panic!()
    };
    let PartitionContent::Fs {
        records: records2, ..
    } = &m2.partitions[0].content
    else {
        panic!()
    };

    assert_eq!(
        records1.len(),
        records2.len(),
        "both runs must produce the same number of records"
    );

    // Patch SHAs must match for changed files.
    for r1 in records1 {
        if let Some(Patch::Real(pref1)) = &r1.patch {
            let r2 = records2
                .iter()
                .find(|r| r.new_path == r1.new_path)
                .expect("matching record must exist in second run");
            if let Some(Patch::Real(pref2)) = &r2.patch {
                assert_eq!(
                    pref1.sha256, pref2.sha256,
                    "patch SHA-256 must be identical across two compress runs of the same content"
                );
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Empty target directory
// ─────────────────────────────────────────────────────────────────────────────

/// Compressing an empty target against a non-empty base must produce a manifest
/// with only deletion records (every base file is removed).
#[tokio::test]
async fn test_compress_empty_target_all_deletions() {
    let storage = FakeStorage::new();
    let base_dir = TempDir::new().unwrap();
    let target_dir = TempDir::new().unwrap(); // empty

    write(base_dir.path(), "etc/a", b"file a");
    write(base_dir.path(), "etc/b", b"file b");

    let compressor = make_compressor(&storage);
    let stats = compressor
        .compress(
            base_dir.path(),
            target_dir.path(),
            base_options("img-empty", Some("img-base")),
        )
        .await
        .unwrap();

    assert_eq!(
        stats.files_removed, 2,
        "both base files must be recorded as deleted"
    );
    assert_eq!(stats.files_added, 0);
    assert_eq!(stats.files_patched, 0);

    let raw = storage.download_manifest("img-empty").await.unwrap();
    let m = Manifest::from_bytes(&raw).unwrap();
    let PartitionContent::Fs { records, .. } = &m.partitions[0].content else {
        panic!()
    };

    let deletions: Vec<_> = records
        .iter()
        .filter(|r| {
            r.old_path.is_some()
                && r.new_path.is_none()
                && !matches!(
                    r.entry_type,
                    image_delta_core::manifest::EntryType::Directory
                )
        })
        .collect();
    assert_eq!(
        deletions.len(),
        2,
        "manifest must contain exactly 2 deletion records"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Error handling
// ─────────────────────────────────────────────────────────────────────────────

/// If `compress()` fails (e.g. target directory does not exist), the image
/// status must be set to `"failed: <reason>"` rather than left at "compressing".
#[tokio::test]
async fn test_compress_error_sets_failed_status() {
    let storage = FakeStorage::new();
    let base_dir = TempDir::new().unwrap();

    let nonexistent = base_dir.path().join("does_not_exist");

    let compressor = make_compressor(&storage);
    let err = compressor
        .compress(
            base_dir.path(),
            &nonexistent,
            base_options("img-fail", None),
        )
        .await;

    assert!(err.is_err(), "compress() must fail for a missing target");

    let status = storage.image_status("img-fail").unwrap_or_default();
    assert!(
        status.starts_with("failed:"),
        "status must start with 'failed:' but was {:?}",
        status
    );
}
