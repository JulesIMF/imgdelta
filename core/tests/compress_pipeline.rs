// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Integration tests for the eight-stage compress_pipeline functions

mod common;

use std::path::PathBuf;

use common::FakeStorage;
use image_delta_core::compress::{
    compute_patches, download_blobs_for_patches, pack_and_upload_archive, s3_lookup,
    upload_lazy_blobs, walkdir, FsDraft,
};
use image_delta_core::manifest::{
    BlobRef, Data, DataRef, EntryType, PartitionContent, Patch, Record,
};
use image_delta_core::partition::{PartitionDescriptor, PartitionKind};
use image_delta_core::{Storage, Xdelta3Encoder};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tempfile::TempDir;
use uuid::Uuid;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn write(dir: &std::path::Path, rel: &str, content: &[u8]) {
    let full = dir.join(rel);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(full, content).unwrap();
}

fn xdelta3_router() -> image_delta_core::routing::RouterEncoder {
    image_delta_core::routing::RouterEncoder::new(vec![], Arc::new(Xdelta3Encoder::new()))
}

fn simple_descriptor() -> PartitionDescriptor {
    PartitionDescriptor {
        number: 1,
        partition_guid: None,
        type_guid: None,
        name: None,
        start_lba: 0,
        end_lba: 0,
        size_bytes: 0,
        flags: 0,
        kind: PartitionKind::Fs {
            fs_type: "ext4".into(),
        },
    }
}

// ── Stage 2: s3_lookup ────────────────────────────────────────────────────────

#[tokio::test]
async fn test_s3_lookup_exact_path_match() {
    let storage = FakeStorage::new();

    // Pre-seed a blob from base image at path "usr/lib/libz.so.1".
    let content = b"original libz content from base image";
    let sha256 = hex::encode(Sha256::digest(content));
    let blob_id = storage.upload_blob(&sha256, content).await.unwrap();
    storage.register_blob_origin("img-base", blob_id, "usr/lib/libz.so.1");

    // Draft: added file at same path with different content.
    let target_dir = TempDir::new().unwrap();
    write(
        target_dir.path(),
        "usr/lib/libz.so.1",
        b"updated libz content",
    );

    let mut draft = FsDraft::default();
    draft.records.push(Record {
        old_path: None,
        new_path: Some("usr/lib/libz.so.1".into()),
        entry_type: EntryType::File,
        size: 20,
        data: Some(Data::LazyBlob(target_dir.path().join("usr/lib/libz.so.1"))),
        patch: None,
        metadata: None,
    });

    let draft = s3_lookup(draft, &storage, "img-base", None).await.unwrap();

    let record = &draft.records[0];
    // data should become BlobRef (delta base from S3).
    assert!(
        matches!(record.data, Some(Data::BlobRef(_))),
        "expected Data::BlobRef after s3_lookup, got {:?}",
        record.data
    );
    // patch should become Lazy { old: BlobRef, new: FilePath }.
    assert!(
        matches!(
            &record.patch,
            Some(Patch::Lazy {
                old_data: DataRef::BlobRef(_),
                new_data: DataRef::FilePath(_),
            })
        ),
        "expected Lazy patch with BlobRef old side, got {:?}",
        record.patch
    );
}

#[tokio::test]
async fn test_s3_lookup_no_candidates_is_noop() {
    let storage = FakeStorage::new();
    // No blobs in base image.

    let target_dir = TempDir::new().unwrap();
    write(target_dir.path(), "etc/config", b"config content");

    let mut draft = FsDraft::default();
    draft.records.push(Record {
        old_path: None,
        new_path: Some("etc/config".into()),
        entry_type: EntryType::File,
        size: 14,
        data: Some(Data::LazyBlob(target_dir.path().join("etc/config"))),
        patch: None,
        metadata: None,
    });

    let draft = s3_lookup(draft, &storage, "img-base", None).await.unwrap();

    assert!(
        matches!(draft.records[0].data, Some(Data::LazyBlob(_))),
        "LazyBlob should be unchanged when no S3 candidates"
    );
    assert!(draft.records[0].patch.is_none());
}

#[tokio::test]
async fn test_s3_lookup_only_matches_added_files() {
    let storage = FakeStorage::new();
    let content = b"base content";
    let sha256 = hex::encode(Sha256::digest(content));
    let blob_id = storage.upload_blob(&sha256, content).await.unwrap();
    storage.register_blob_origin("img-base", blob_id, "lib/libfoo.so.1");

    // Draft: one changed file (old_path=Some) and one added file.
    let target_dir = TempDir::new().unwrap();
    write(target_dir.path(), "lib/libfoo.so.1", b"changed content");

    let mut draft = FsDraft::default();
    // Changed file (should not be affected by s3_lookup).
    draft.records.push(Record {
        old_path: Some("lib/libfoo.so.1".into()),
        new_path: Some("lib/libfoo.so.1".into()),
        entry_type: EntryType::File,
        size: 15,
        data: None,
        patch: Some(Patch::Lazy {
            old_data: DataRef::FilePath("/mnt/base/lib/libfoo.so.1".into()),
            new_data: DataRef::FilePath(target_dir.path().join("lib/libfoo.so.1")),
        }),
        metadata: None,
    });

    let draft = s3_lookup(draft, &storage, "img-base", None).await.unwrap();

    // The changed record must not be altered by s3_lookup.
    assert!(
        matches!(draft.records[0].patch, Some(Patch::Lazy { .. })),
        "changed file's Lazy patch must not be modified by s3_lookup"
    );
    assert!(draft.records[0].data.is_none());
}

// ── Stage 5: upload_lazy_blobs ────────────────────────────────────────────────

#[tokio::test]
async fn test_upload_lazy_blobs_new_file() {
    let storage = FakeStorage::new();
    let target_dir = TempDir::new().unwrap();
    write(
        target_dir.path(),
        "usr/bin/newcmd",
        b"executable content here",
    );

    let mut draft = FsDraft::default();
    draft.records.push(Record {
        old_path: None,
        new_path: Some("usr/bin/newcmd".into()),
        entry_type: EntryType::File,
        size: 23,
        data: Some(Data::LazyBlob(target_dir.path().join("usr/bin/newcmd"))),
        patch: None,
        metadata: None,
    });

    let draft = upload_lazy_blobs(draft, &storage, "img-001", None, None)
        .await
        .unwrap();

    assert!(
        matches!(draft.records[0].data, Some(Data::BlobRef(_))),
        "LazyBlob should be replaced with BlobRef after upload"
    );
}

#[tokio::test]
async fn test_upload_lazy_blobs_sha256_dedup() {
    let storage = FakeStorage::new();
    let target_dir = TempDir::new().unwrap();

    let content = b"shared content for both files";
    write(target_dir.path(), "a/file1.txt", content);
    write(target_dir.path(), "b/file2.txt", content);

    let mut draft = FsDraft::default();
    for (path, rel) in [
        (target_dir.path().join("a/file1.txt"), "a/file1.txt"),
        (target_dir.path().join("b/file2.txt"), "b/file2.txt"),
    ] {
        draft.records.push(Record {
            old_path: None,
            new_path: Some(rel.into()),
            entry_type: EntryType::File,
            size: content.len() as u64,
            data: Some(Data::LazyBlob(path)),
            patch: None,
            metadata: None,
        });
    }

    let draft = upload_lazy_blobs(draft, &storage, "img-001", None, None)
        .await
        .unwrap();

    // Both files should have the same blob_id (SHA-256 dedup).
    let ids: Vec<Uuid> = draft
        .records
        .iter()
        .filter_map(|r| match &r.data {
            Some(Data::BlobRef(br)) => Some(br.blob_id),
            _ => None,
        })
        .collect();

    assert_eq!(ids.len(), 2, "both records should have BlobRef");
    assert_eq!(ids[0], ids[1], "same content → same blob_id (dedup)");
}

#[tokio::test]
async fn test_upload_lazy_blobs_skips_non_lazy() {
    let storage = FakeStorage::new();

    let mut draft = FsDraft::default();
    // A BlobRef record (already uploaded) — must not be touched.
    draft.records.push(Record {
        old_path: None,
        new_path: Some("etc/config".into()),
        entry_type: EntryType::File,
        size: 100,
        data: Some(Data::BlobRef(BlobRef {
            blob_id: Uuid::nil(),
            size: 100,
        })),
        patch: None,
        metadata: None,
    });

    let before = draft.records[0].data.clone();
    let draft = upload_lazy_blobs(draft, &storage, "img-001", None, None)
        .await
        .unwrap();

    assert_eq!(
        draft.records[0].data, before,
        "BlobRef record must not be changed by upload_lazy_blobs"
    );
}

// ── Stage 6: download_blobs_for_patches ──────────────────────────────────────

#[tokio::test]
async fn test_download_blobs_for_patches_replaces_blob_ref() {
    let storage = FakeStorage::new();
    let content = b"base file content for delta";
    let sha256 = hex::encode(Sha256::digest(content));
    let blob_id = storage.upload_blob(&sha256, content).await.unwrap();

    let target_dir = TempDir::new().unwrap();
    write(target_dir.path(), "lib/lib.so", b"new lib content");

    let mut draft = FsDraft::default();
    draft.records.push(Record {
        old_path: Some("lib/lib.so".into()),
        new_path: Some("lib/lib.so".into()),
        entry_type: EntryType::File,
        size: 15,
        data: Some(Data::BlobRef(BlobRef {
            blob_id,
            size: content.len() as u64,
        })),
        patch: Some(Patch::Lazy {
            old_data: DataRef::BlobRef(BlobRef {
                blob_id,
                size: content.len() as u64,
            }),
            new_data: DataRef::FilePath(target_dir.path().join("lib/lib.so")),
        }),
        metadata: None,
    });

    let tmp_dir = TempDir::new().unwrap();
    let draft = download_blobs_for_patches(draft, &storage, tmp_dir.path())
        .await
        .unwrap();

    // BlobRef in old_data must become FilePath.
    assert!(
        matches!(
            &draft.records[0].patch,
            Some(Patch::Lazy {
                old_data: DataRef::FilePath(_),
                ..
            })
        ),
        "BlobRef in old_data should be replaced with FilePath after download"
    );
    // The downloaded file must exist on disk.
    assert!(!draft.tmp_files.is_empty(), "tmp_files must be populated");
    for p in &draft.tmp_files {
        assert!(p.exists(), "tmp file {p:?} must exist on disk");
        assert_eq!(std::fs::read(p).unwrap(), content);
    }
}

#[tokio::test]
async fn test_download_blobs_for_patches_dedup_within_call() {
    let storage = FakeStorage::new();
    let content = b"shared base blob";
    let sha256 = hex::encode(Sha256::digest(content));
    let blob_id = storage.upload_blob(&sha256, content).await.unwrap();

    let target_dir = TempDir::new().unwrap();
    write(target_dir.path(), "a.txt", b"new a");
    write(target_dir.path(), "b.txt", b"new b");

    let mut draft = FsDraft::default();
    for (new_path, tgt) in [("a.txt", "a.txt"), ("b.txt", "b.txt")] {
        draft.records.push(Record {
            old_path: Some(new_path.into()),
            new_path: Some(new_path.into()),
            entry_type: EntryType::File,
            size: 5,
            data: None,
            patch: Some(Patch::Lazy {
                old_data: DataRef::BlobRef(BlobRef {
                    blob_id,
                    size: content.len() as u64,
                }),
                new_data: DataRef::FilePath(target_dir.path().join(tgt)),
            }),
            metadata: None,
        });
    }

    let tmp_dir = TempDir::new().unwrap();
    let draft = download_blobs_for_patches(draft, &storage, tmp_dir.path())
        .await
        .unwrap();

    // Both patches should reference the same temp file (dedup).
    let paths: Vec<PathBuf> = draft
        .records
        .iter()
        .filter_map(|r| match &r.patch {
            Some(Patch::Lazy {
                old_data: DataRef::FilePath(p),
                ..
            }) => Some(p.clone()),
            _ => None,
        })
        .collect();

    assert_eq!(paths.len(), 2);
    assert_eq!(paths[0], paths[1], "same blob_id → same tmp file path");
    // Only one file downloaded.
    assert_eq!(draft.tmp_files.len(), 1);
}

// ── Stage 8: pack_and_upload_archive ─────────────────────────────────────────

#[tokio::test]
async fn test_pack_upload_archive_produces_fs_content() {
    let storage = FakeStorage::new();

    let base_dir = TempDir::new().unwrap();
    let target_dir = TempDir::new().unwrap();
    write(base_dir.path(), "etc/config", b"old config content here!");
    write(target_dir.path(), "etc/config", b"new config content here!");

    // Build a draft with one computed patch.
    let mut draft = walkdir(base_dir.path(), target_dir.path()).unwrap();
    // Stage 5: no lazy blobs (changed file, no new additions).
    let router = xdelta3_router();
    draft = compute_patches(draft, &router, 1).unwrap();

    let (content, _compressed, _archive_bytes) =
        pack_and_upload_archive(draft, &storage, "img-001", "ext4")
            .await
            .unwrap();

    // Must return PartitionContent::Fs.
    let PartitionContent::Fs { fs_type, records } = content else {
        panic!("expected PartitionContent::Fs");
    };
    assert_eq!(fs_type, "ext4");

    // Changed record must have a Real patch.
    let changed = records
        .iter()
        .find(|r| r.new_path.as_deref() == Some("etc/config"))
        .unwrap();
    assert!(
        matches!(changed.patch, Some(Patch::Real(_))),
        "changed file must have Patch::Real after full pipeline"
    );

    // Patches archive must be uploaded to storage.
    assert!(
        storage.has_patches("img-001"),
        "patches archive must be uploaded"
    );
}

// ── Full pipeline: compress_fs_partition ──────────────────────────────────────

/// Golden test: compress two directory versions → verify manifest structure.
///
/// base contains: etc/passwd (v1), etc/shadow (unchanged), lib/libz.so.1
/// target contains: etc/passwd (v2), etc/shadow (unchanged), lib/libz.so.2 (renamed libz.so.1)
///
/// After compression:
/// - etc/passwd: changed → Patch::Real (xdelta3)
/// - etc/shadow: absent (unchanged → no record)
/// - lib/libz.so.1: removed → deletion record
/// - lib/libz.so.2: renamed from libz.so.1 OR added
#[tokio::test]
async fn test_compress_fs_partition_golden() {
    let storage = Arc::new(FakeStorage::new());

    let base_dir = TempDir::new().unwrap();
    let target_dir = TempDir::new().unwrap();

    let passwd_v1 = b"root:x:0:0:root:/root:/bin/bash\nuser:x:1000:1000::/home/user:/bin/sh\n";
    let passwd_v2 = b"root:x:0:0:root:/root:/bin/bash\nuser:x:1001:1001::/home/user:/bin/sh\n";
    let shadow = b"root:!:19000:0:99999:7:::\n";
    let libz_content = b"ELF fake libz content padded to make xdelta3 worthwhile 000000000000";

    write(base_dir.path(), "etc/passwd", passwd_v1);
    write(base_dir.path(), "etc/shadow", shadow);
    write(base_dir.path(), "lib/libz.so.1", libz_content);

    write(target_dir.path(), "etc/passwd", passwd_v2);
    write(target_dir.path(), "etc/shadow", shadow); // unchanged
    write(target_dir.path(), "lib/libz.so.2", libz_content); // renamed
    write(
        target_dir.path(),
        "usr/bin/newutil",
        b"brand new binary added in v2",
    ); // added

    // Align mtime on shadow so it doesn't appear as changed.
    let shadow_mtime = base_dir
        .path()
        .join("etc/shadow")
        .symlink_metadata()
        .unwrap()
        .modified()
        .unwrap();
    filetime::set_file_mtime(
        target_dir.path().join("etc/shadow"),
        filetime::FileTime::from_system_time(shadow_mtime),
    )
    .unwrap();

    let descriptor = simple_descriptor();
    let router = Arc::new(xdelta3_router());

    let (partition_manifest, _compressed, _archive_bytes) =
        image_delta_core::compress::compress_fs_partition(
            base_dir.path(),
            target_dir.path(),
            &descriptor,
            Arc::clone(&storage) as Arc<dyn image_delta_core::Storage>,
            "img-002",
            Some("img-001"),
            router,
            "ext4",
            1,
        )
        .await
        .unwrap();

    let PartitionContent::Fs { fs_type, records } = &partition_manifest.content else {
        panic!("expected PartitionContent::Fs");
    };
    assert_eq!(fs_type, "ext4");

    // etc/passwd must be changed in-place.
    let passwd_record = records
        .iter()
        .find(|r| r.new_path.as_deref() == Some("etc/passwd"))
        .expect("etc/passwd must appear in manifest");
    assert!(
        matches!(passwd_record.patch, Some(Patch::Real(_))),
        "etc/passwd must have a Real patch"
    );

    // etc/shadow must NOT appear (unchanged).
    assert!(
        records
            .iter()
            .all(|r| r.new_path.as_deref() != Some("etc/shadow")),
        "unchanged etc/shadow must not appear in manifest"
    );

    // lib/libz.so.2 must appear (new or renamed).
    assert!(
        records
            .iter()
            .any(|r| r.new_path.as_deref() == Some("lib/libz.so.2")),
        "lib/libz.so.2 must appear in manifest"
    );

    // All patches must be Real (no Lazy patches remain).
    for r in records {
        assert!(
            !matches!(r.patch, Some(Patch::Lazy { .. })),
            "Lazy patch must not appear in finalised manifest: {r:?}"
        );
    }

    // Manifest archive must be uploaded to storage.
    assert!(storage.has_patches("img-002"), "patches must be uploaded");
    assert!(
        storage.uploaded_blob_count() > 0,
        "at least one blob must be uploaded"
    );
}

/// Verify that the full compress pipeline followed by manifest serialisation
/// produces a manifest that round-trips through MessagePack without loss.
#[tokio::test]
async fn test_compress_manifest_serialisation_roundtrip() {
    use image_delta_core::manifest::{Manifest, ManifestHeader, MANIFEST_VERSION};
    use image_delta_core::partition::{DiskLayout, DiskScheme};

    let storage = FakeStorage::new();
    let base_dir = TempDir::new().unwrap();
    let target_dir = TempDir::new().unwrap();

    write(base_dir.path(), "usr/bin/tool", b"tool v1.0");
    write(target_dir.path(), "usr/bin/tool", b"tool v1.1");

    let descriptor = simple_descriptor();
    let router = Arc::new(xdelta3_router());

    let (partition_manifest, _compressed, _archive_bytes) =
        image_delta_core::compress::compress_fs_partition(
            base_dir.path(),
            target_dir.path(),
            &descriptor,
            Arc::new(storage) as Arc<dyn image_delta_core::Storage>,
            "img-rt",
            None,
            router,
            "ext4",
            1,
        )
        .await
        .unwrap();

    let manifest = Manifest {
        header: ManifestHeader {
            version: MANIFEST_VERSION,
            image_id: "img-rt".into(),
            base_image_id: None,
            format: "directory".into(),
            created_at: 1_714_000_000,
            patches_compressed: false,
        },
        disk_layout: DiskLayout {
            scheme: DiskScheme::SingleFs,
            disk_guid: None,
            partitions: vec![],
        },
        partitions: vec![partition_manifest],
    };

    let bytes = rmp_serde::to_vec_named(&manifest).unwrap();
    let recovered: Manifest = rmp_serde::from_slice(&bytes).unwrap();
    assert_eq!(recovered.header.image_id, "img-rt");
    assert_eq!(recovered.partitions.len(), 1);
}

// ── Stage 5: upload_lazy_blobs — dedup avoids redundant upload ────────────────

/// When two files have identical content, `blob_exists` must be called for both
/// but `upload_blob` must be called only once (the second call is skipped via
/// the SHA-256 dedup check in `upload_lazy_blobs`).
#[tokio::test]
async fn test_upload_lazy_blobs_dedup_skips_upload() {
    let storage = FakeStorage::new();
    let target_dir = TempDir::new().unwrap();

    let content = b"identical content in both files";
    write(target_dir.path(), "x/a.txt", content);
    write(target_dir.path(), "x/b.txt", content);

    let mut draft = FsDraft::default();
    for rel in ["x/a.txt", "x/b.txt"] {
        draft.records.push(Record {
            old_path: None,
            new_path: Some(rel.into()),
            entry_type: EntryType::File,
            size: content.len() as u64,
            data: Some(Data::LazyBlob(target_dir.path().join(rel))),
            patch: None,
            metadata: None,
        });
    }

    let _draft = upload_lazy_blobs(draft, &storage, "img-dedup", None, None)
        .await
        .unwrap();

    assert_eq!(
        storage.blob_exists_call_count(),
        2,
        "blob_exists must be called once per file"
    );
    assert_eq!(
        storage.upload_call_count(),
        1,
        "upload_blob must be called only once for identical content"
    );
    assert_eq!(
        storage.uploaded_blob_count(),
        1,
        "only one distinct blob must be stored"
    );
}

// ── FakeStorage: concurrent upload safety ─────────────────────────────────────

/// Spawn 30 tokio tasks that concurrently upload unique blobs to the same
/// FakeStorage.  The Mutex must prevent any data races and all 30 blobs must
/// be correctly stored without corruption.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_fake_storage_concurrent_uploads_no_corruption() {
    let storage = FakeStorage::new();

    let mut handles = Vec::new();
    for i in 0..30u32 {
        let s = storage.clone();
        handles.push(tokio::spawn(async move {
            // Each task has unique content so no dedup occurs.
            let content: Vec<u8> = i.to_le_bytes().iter().chain(b"padding").copied().collect();
            let sha256 = hex::encode(sha2::Sha256::digest(&content));
            s.upload_blob(&sha256, &content).await.unwrap()
        }));
    }

    let mut ids = Vec::new();
    for h in handles {
        ids.push(h.await.expect("task must not panic"));
    }

    assert_eq!(
        storage.uploaded_blob_count(),
        30,
        "all 30 unique blobs must be stored"
    );
    // All returned UUIDs must be distinct (no silent collision).
    let unique: std::collections::HashSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), 30, "every upload must return a distinct UUID");
}

/// Five concurrent `upload_lazy_blobs` calls (via `tokio::join!`) sharing the
/// same FakeStorage must upload exactly 50 distinct blobs with no corruption.
#[tokio::test]
async fn test_upload_lazy_blobs_concurrent_batches() {
    let storage = FakeStorage::new();
    let target_dir = TempDir::new().unwrap();

    // Create 50 files with unique content.
    for i in 0u32..50 {
        let content = format!("unique content for file number {i:03}");
        write(
            target_dir.path(),
            &format!("f{i:03}.bin"),
            content.as_bytes(),
        );
    }

    let make_draft = |start: u32| {
        let mut draft = FsDraft::default();
        for i in start..start + 10 {
            let rel = format!("f{i:03}.bin");
            draft.records.push(Record {
                old_path: None,
                new_path: Some(rel.clone()),
                entry_type: EntryType::File,
                size: 0,
                data: Some(Data::LazyBlob(target_dir.path().join(&rel))),
                patch: None,
                metadata: None,
            });
        }
        draft
    };

    let (r0, r1, r2, r3, r4) = tokio::join!(
        upload_lazy_blobs(make_draft(0), &storage, "img-conc", None, None),
        upload_lazy_blobs(make_draft(10), &storage, "img-conc", None, None),
        upload_lazy_blobs(make_draft(20), &storage, "img-conc", None, None),
        upload_lazy_blobs(make_draft(30), &storage, "img-conc", None, None),
        upload_lazy_blobs(make_draft(40), &storage, "img-conc", None, None),
    );

    for (batch_idx, draft) in [r0, r1, r2, r3, r4].into_iter().enumerate() {
        let draft = draft.unwrap_or_else(|e| panic!("batch {batch_idx} failed: {e}"));
        for r in &draft.records {
            assert!(
                matches!(r.data, Some(Data::BlobRef(_))),
                "batch {batch_idx}: expected BlobRef, got {:?}",
                r.data
            );
        }
    }

    assert_eq!(
        storage.uploaded_blob_count(),
        50,
        "all 50 unique files must be stored as distinct blobs"
    );
}

// ── Stage 1: walkdir — deep nesting ──────────────────────────────────────────

/// A deeply nested path must appear verbatim (as a relative POSIX path) in the
/// resulting record's `new_path`.
#[test]
fn test_walkdir_deep_nesting_path_preserved() {
    let base_dir = TempDir::new().unwrap();
    let target_dir = TempDir::new().unwrap();

    let deep = "usr/lib/x86_64-linux-gnu/security/pam_unix.so";
    write(target_dir.path(), deep, b"pam module binary content");

    let draft = walkdir(base_dir.path(), target_dir.path()).unwrap();

    let record = draft
        .records
        .iter()
        .find(|r| r.new_path.as_deref() == Some(deep))
        .expect("deeply nested file must appear in draft");

    assert_eq!(record.new_path.as_deref(), Some(deep));
    assert!(
        matches!(record.entry_type, EntryType::File),
        "must be a File record"
    );
    assert!(
        matches!(record.data, Some(Data::LazyBlob(_))),
        "must be LazyBlob for a new file"
    );
}

// ── Stage 1: walkdir — 3-file hardlink group ─────────────────────────────────

/// Three directory entries sharing the same inode must produce exactly one
/// canonical `LazyBlob` record (alphabetically first path) and two
/// `HardlinkTo` records pointing at the canonical.
#[test]
fn test_walkdir_three_hardlinks_canonical_alphabetically_first() {
    let base_dir = TempDir::new().unwrap();
    let target_dir = TempDir::new().unwrap();

    // Create canonical file and two hardlinks.
    let canonical = "a/first.txt";
    write(target_dir.path(), canonical, b"hardlinked content");
    std::fs::create_dir_all(target_dir.path().join("b")).unwrap();
    std::fs::hard_link(
        target_dir.path().join(canonical),
        target_dir.path().join("b/second.txt"),
    )
    .unwrap();
    std::fs::create_dir_all(target_dir.path().join("c")).unwrap();
    std::fs::hard_link(
        target_dir.path().join(canonical),
        target_dir.path().join("c/third.txt"),
    )
    .unwrap();

    let draft = walkdir(base_dir.path(), target_dir.path()).unwrap();

    // Find the canonical record.
    let canon_record = draft
        .records
        .iter()
        .find(|r| r.new_path.as_deref() == Some(canonical))
        .expect("canonical path must appear in draft");
    assert!(
        matches!(canon_record.data, Some(Data::LazyBlob(_))),
        "canonical must be LazyBlob, got {:?}",
        canon_record.data
    );

    // Both non-canonical paths must be HardlinkTo the canonical.
    for non_canon in ["b/second.txt", "c/third.txt"] {
        let r = draft
            .records
            .iter()
            .find(|r| r.new_path.as_deref() == Some(non_canon))
            .unwrap_or_else(|| panic!("{non_canon} must appear in draft"));
        assert!(
            matches!(r.entry_type, EntryType::Hardlink),
            "{non_canon} must be Hardlink, got {:?}",
            r.entry_type
        );
        assert_eq!(
            r.data,
            Some(Data::HardlinkTo(canonical.to_string())),
            "{non_canon} must link to canonical path"
        );
    }
}

// ── Stage 1: walkdir — empty directories ─────────────────────────────────────

/// An empty directory added in target must produce a Directory record with
/// `new_path` set and `data = None`.
#[test]
fn test_walkdir_empty_dir_added() {
    let base_dir = TempDir::new().unwrap();
    let target_dir = TempDir::new().unwrap();

    std::fs::create_dir_all(target_dir.path().join("data/empty")).unwrap();

    let draft = walkdir(base_dir.path(), target_dir.path()).unwrap();

    let record = draft
        .records
        .iter()
        .find(|r| r.new_path.as_deref() == Some("data/empty"))
        .expect("added empty dir must appear in draft");

    assert!(
        matches!(record.entry_type, EntryType::Directory),
        "must be Directory record"
    );
    assert!(record.old_path.is_none(), "added dir must have no old_path");
    assert!(record.data.is_none(), "directory record must have no data");
}

/// An empty directory removed from base must produce a deletion record with
/// `old_path` set and `new_path = None`.
#[test]
fn test_walkdir_empty_dir_removed() {
    let base_dir = TempDir::new().unwrap();
    let target_dir = TempDir::new().unwrap();

    std::fs::create_dir_all(base_dir.path().join("data/old")).unwrap();

    let draft = walkdir(base_dir.path(), target_dir.path()).unwrap();

    let record = draft
        .records
        .iter()
        .find(|r| r.old_path.as_deref() == Some("data/old"))
        .expect("removed empty dir must appear in draft");

    assert!(
        matches!(record.entry_type, EntryType::Directory),
        "must be Directory record"
    );
    assert!(
        record.new_path.is_none(),
        "removed dir must have no new_path"
    );
    assert!(
        record.data.is_none(),
        "directory deletion record must have no data"
    );
}

// ── Stage 7: compute_patches — rayon stress with many files ──────────────────

/// 30 changed files processed in parallel via rayon must each produce a
/// correctly indexed `patch_bytes` entry with no missing or duplicate keys.
#[test]
fn test_compute_patches_rayon_stress_many_files() {
    let n: usize = 30;
    let base_dir = TempDir::new().unwrap();
    let target_dir = TempDir::new().unwrap();

    for i in 0..n {
        let base_content =
            format!("base version 1 content for file {i:03} padding padding padding");
        let target_content =
            format!("target version 2 content for file {i:03} padding padding padding");
        let rel = format!("file_{i:03}.txt");
        write(base_dir.path(), &rel, base_content.as_bytes());
        write(target_dir.path(), &rel, target_content.as_bytes());
    }

    let draft = walkdir(base_dir.path(), target_dir.path()).unwrap();
    assert_eq!(
        draft.records.len(),
        n,
        "must have exactly {n} changed records"
    );

    let router = xdelta3_router();
    let draft = compute_patches(draft, &router, 1).unwrap();

    assert_eq!(
        draft.patch_bytes.len(),
        n,
        "must have one patch_bytes entry per changed file"
    );

    // Verify each record has a Real patch and no Lazy patches remain.
    for (idx, record) in draft.records.iter().enumerate() {
        assert!(
            matches!(
                record.patch,
                Some(image_delta_core::manifest::Patch::Real(_))
            ),
            "record {idx} must have Patch::Real after compute_patches"
        );
        let expected_key = format!("{idx:06}.patch");
        assert!(
            draft.patch_bytes.contains_key(&expected_key),
            "patch_bytes must contain key {expected_key:?}"
        );
    }

    // No duplicate keys (HashMap can't have duplicates, but verify count matches).
    assert_eq!(
        draft.patch_bytes.len(),
        n,
        "patch_bytes must have exactly {n} distinct entries"
    );
}

// ── Stage 8: pack_and_upload_archive — gzip compression flag ─────────────────

/// Highly repetitive patch content compresses very well; the archive must be
/// uploaded with `compressed = true`.  This exercises the `try_gzip` path in
/// `pack_and_upload_archive` that chooses gzip when the compressed form is
/// smaller than the original.
#[tokio::test]
async fn test_pack_upload_archive_compressible_data_uses_gzip() {
    let storage = FakeStorage::new();

    // 64 KiB of repetitive bytes — gzip compresses these to a tiny fraction.
    let repetitive: Vec<u8> =
        b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".repeat(1161); // ≈ 64 KiB

    let sha256 = hex::encode(sha2::Sha256::digest(&repetitive));
    let mut draft = FsDraft::default();
    let pref = image_delta_core::manifest::PatchRef {
        archive_entry: "000000.patch".to_string(),
        sha256,
        algorithm_code: image_delta_core::AlgorithmCode::Passthrough,
        algorithm_id: None,
    };
    draft.records.push(Record {
        old_path: None,
        new_path: Some("data/blob".into()),
        entry_type: EntryType::File,
        size: repetitive.len() as u64,
        data: None,
        patch: Some(image_delta_core::manifest::Patch::Real(pref)),
        metadata: None,
    });
    draft
        .patch_bytes
        .insert("000000.patch".to_string(), repetitive);

    let (_content, _, _) = pack_and_upload_archive(draft, &storage, "img-comp", "ext4")
        .await
        .unwrap();

    assert!(storage.has_patches("img-comp"), "patches must be uploaded");
    assert_eq!(
        storage.patches_were_compressed("img-comp"),
        Some(true),
        "highly compressible repetitive data must be gzip-compressed"
    );
}

// ── Full pipeline: first compression (base = None, many new files) ────────────

/// When there is no base image (`base_image_id = None`), all files in target
/// must be uploaded as blobs (`Data::BlobRef`) with no `Patch::Lazy` remaining
/// in the final manifest.
#[tokio::test]
async fn test_compress_fs_partition_first_compression_many_new_files() {
    let storage = Arc::new(FakeStorage::new());

    let base_dir = TempDir::new().unwrap(); // empty — no base files
    let target_dir = TempDir::new().unwrap();

    let file_count: u32 = 12;
    for i in 0..file_count {
        let content = format!("brand new file content number {i:04} added in this image");
        write(
            target_dir.path(),
            &format!("usr/share/data/file_{i:04}.txt"),
            content.as_bytes(),
        );
    }

    let descriptor = simple_descriptor();
    let router = Arc::new(xdelta3_router());

    let (partition_manifest, _compressed, _archive_bytes) =
        image_delta_core::compress::compress_fs_partition(
            base_dir.path(),
            target_dir.path(),
            &descriptor,
            Arc::clone(&storage) as Arc<dyn image_delta_core::Storage>,
            "img-first",
            None, // no base image — first compression
            router,
            "ext4",
            1,
        )
        .await
        .unwrap();

    let image_delta_core::manifest::PartitionContent::Fs { records, .. } =
        &partition_manifest.content
    else {
        panic!("expected PartitionContent::Fs");
    };

    // Every record for a regular file must have Data::BlobRef.
    let file_records: Vec<_> = records
        .iter()
        .filter(|r| matches!(r.entry_type, EntryType::File))
        .collect();
    assert_eq!(
        file_records.len(),
        file_count as usize,
        "all {file_count} new files must appear in manifest"
    );
    for r in &file_records {
        assert!(
            matches!(r.data, Some(Data::BlobRef(_))),
            "new file must have Data::BlobRef after pipeline: {:?}",
            r.data
        );
    }

    // No Lazy patches must remain.
    for r in records {
        assert!(
            !matches!(
                r.patch,
                Some(image_delta_core::manifest::Patch::Lazy { .. })
            ),
            "Lazy patch must not appear in finalised manifest: {r:?}"
        );
    }

    // All distinct files must be uploaded as blobs (no base → no dedup possible).
    assert_eq!(
        storage.uploaded_blob_count(),
        file_count as usize,
        "each unique new file must produce one stored blob"
    );

    // Patches archive must be uploaded (even if it contains 0 patches,
    // the tar is still created and uploaded by pack_and_upload_archive).
    assert!(
        storage.has_patches("img-first"),
        "patches archive must be uploaded even for a first compression"
    );
}
