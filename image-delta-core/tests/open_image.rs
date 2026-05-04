// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Integration tests for Image::open() and PartitionHandle variants

use image_delta_core::partition::{DiskScheme, PartitionDescriptor};
/// Tests for [`OpenImage`] / [`PartitionHandle`] — Phase 6.C.
///
/// All tests here run without cloud or qcow2 support: they exercise
/// [`DirectoryImage::open`] and the handle types in isolation.
use image_delta_core::{
    BiosBootHandle, DirectoryImage, FsHandle, Image, PartitionHandle, PartitionKind, RawHandle,
};
use std::fs;
use tempfile::TempDir;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn write(dir: &std::path::Path, rel: &str, content: &[u8]) {
    let full = dir.join(rel);
    if let Some(p) = full.parent() {
        fs::create_dir_all(p).unwrap();
    }
    fs::write(full, content).unwrap();
}

fn make_test_dir() -> TempDir {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "etc/os-release", b"ID=testlinux\n");
    write(dir.path(), "usr/bin/sh", b"ELF stub");
    dir
}

// ── DirectoryImage::open() ────────────────────────────────────────────────────

/// Opening a valid directory returns `Ok`.
#[test]
fn test_directory_open_succeeds() {
    let dir = make_test_dir();
    let img = DirectoryImage::new();
    img.open(dir.path())
        .expect("open must succeed for an existing directory");
}

/// Opening a non-existent path returns `Err`.
#[test]
fn test_directory_open_nonexistent_fails() {
    let img = DirectoryImage::new();
    let result = img.open(std::path::Path::new(
        "/nonexistent/path/that/does/not/exist",
    ));
    assert!(result.is_err(), "open of nonexistent path must fail");
}

/// Opening a file (not a directory) returns `Err`.
#[test]
fn test_directory_open_file_fails() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("image.raw");
    fs::write(&file, b"not a directory").unwrap();
    let img = DirectoryImage::new();
    assert!(img.open(&file).is_err(), "open of a file must fail");
}

/// `disk_layout().scheme` is `SingleFs` for a directory image.
#[test]
fn test_directory_open_disk_layout_is_single_fs() {
    let dir = make_test_dir();
    let img = DirectoryImage::new();
    let open = img.open(dir.path()).unwrap();
    assert_eq!(
        open.disk_layout().scheme,
        DiskScheme::SingleFs,
        "DirectoryImage must report SingleFs disk scheme"
    );
    assert!(
        open.disk_layout().disk_guid.is_none(),
        "DirectoryImage must not have a disk GUID"
    );
}

/// `partitions()` returns exactly one partition for a directory image.
#[test]
fn test_directory_open_single_partition() {
    let dir = make_test_dir();
    let img = DirectoryImage::new();
    let open = img.open(dir.path()).unwrap();
    let partitions = open.partitions().expect("partitions() must succeed");
    assert_eq!(
        partitions.len(),
        1,
        "DirectoryImage must have exactly one partition"
    );
}

/// The single partition is `PartitionHandle::Fs`.
#[test]
fn test_directory_open_partition_is_fs() {
    let dir = make_test_dir();
    let img = DirectoryImage::new();
    let open = img.open(dir.path()).unwrap();
    let mut partitions = open.partitions().unwrap();
    let handle = partitions.remove(0);
    assert!(
        matches!(handle, PartitionHandle::Fs(_)),
        "DirectoryImage partition must be PartitionHandle::Fs"
    );
}

/// `PartitionHandle::descriptor()` for the directory partition has `number == 1`.
#[test]
fn test_directory_partition_descriptor_number() {
    let dir = make_test_dir();
    let img = DirectoryImage::new();
    let open = img.open(dir.path()).unwrap();
    let partitions = open.partitions().unwrap();
    let descriptor = partitions[0].descriptor();
    assert_eq!(descriptor.number, 1);
}

/// The `Fs` partition's `kind` is `Fs { fs_type: "directory" }`.
#[test]
fn test_directory_partition_kind_is_fs_directory() {
    let dir = make_test_dir();
    let img = DirectoryImage::new();
    let open = img.open(dir.path()).unwrap();
    let mut partitions = open.partitions().unwrap();
    let PartitionHandle::Fs(fs) = partitions.remove(0) else {
        panic!("expected Fs handle");
    };
    assert!(
        matches!(
            &fs.descriptor.kind,
            PartitionKind::Fs { fs_type } if fs_type == "directory"
        ),
        "fs_type must be 'directory', got {:?}",
        fs.descriptor.kind
    );
}

/// `FsHandle::mount()` returns a `MountHandle` whose root is the opened path.
#[test]
fn test_directory_fs_handle_mount_root_matches_path() {
    let dir = make_test_dir();
    let img = DirectoryImage::new();
    let open = img.open(dir.path()).unwrap();
    let mut partitions = open.partitions().unwrap();
    let PartitionHandle::Fs(fs) = partitions.remove(0) else {
        panic!("expected Fs handle");
    };
    let mount = fs.mount().expect("mount must succeed for a directory");
    assert_eq!(
        mount.root(),
        dir.path(),
        "mount root must match the opened directory path"
    );
}

/// Files inside the directory are accessible through the mounted root.
#[test]
fn test_directory_mount_files_accessible() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "etc/os-release", b"ID=testlinux\n");
    write(dir.path(), "usr/bin/sh", b"ELF stub");

    let img = DirectoryImage::new();
    let open = img.open(dir.path()).unwrap();
    let mut partitions = open.partitions().unwrap();
    let PartitionHandle::Fs(fs) = partitions.remove(0) else {
        panic!("expected Fs handle");
    };
    let mount = fs.mount().unwrap();

    assert_eq!(
        fs::read(mount.root().join("etc/os-release")).unwrap(),
        b"ID=testlinux\n"
    );
    assert_eq!(
        fs::read(mount.root().join("usr/bin/sh")).unwrap(),
        b"ELF stub"
    );
}

/// `mount()` can be called multiple times on the same `FsHandle`.
#[test]
fn test_directory_fs_handle_mount_is_repeatable() {
    let dir = make_test_dir();
    let img = DirectoryImage::new();
    let open = img.open(dir.path()).unwrap();
    let mut partitions = open.partitions().unwrap();
    let PartitionHandle::Fs(fs) = partitions.remove(0) else {
        panic!("expected Fs handle");
    };

    let m1 = fs.mount().expect("first mount");
    let m2 = fs.mount().expect("second mount");
    assert_eq!(
        m1.root(),
        m2.root(),
        "both mounts must point to the same root"
    );
}

/// `partitions()` can be called multiple times and always returns the same layout.
#[test]
fn test_directory_partitions_is_idempotent() {
    let dir = make_test_dir();
    let img = DirectoryImage::new();
    let open = img.open(dir.path()).unwrap();

    let p1 = open.partitions().unwrap();
    let p2 = open.partitions().unwrap();
    assert_eq!(p1.len(), p2.len());
    assert_eq!(p1[0].descriptor().number, p2[0].descriptor().number);
}

// ── PartitionHandle helpers ───────────────────────────────────────────────────

/// `PartitionHandle::descriptor()` returns the correct descriptor for each variant.
#[test]
fn test_partition_handle_descriptor_accessor() {
    fn make_desc(n: u32) -> PartitionDescriptor {
        PartitionDescriptor {
            number: n,
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

    let fs = PartitionHandle::Fs(FsHandle::new(make_desc(1), || {
        panic!("mount_fn should not be called in this test")
    }));
    let bios = PartitionHandle::BiosBoot(BiosBootHandle::new(make_desc(2), || {
        panic!("read_fn should not be called")
    }));
    let raw = PartitionHandle::Raw(RawHandle::new(make_desc(3), || {
        panic!("read_fn should not be called")
    }));

    assert_eq!(fs.descriptor().number, 1);
    assert_eq!(bios.descriptor().number, 2);
    assert_eq!(raw.descriptor().number, 3);
}

/// `BiosBootHandle::read_raw()` returns the bytes provided by the closure.
#[test]
fn test_bios_boot_handle_read_raw() {
    let expected = b"GRUB stage1 bytes".to_vec();
    let expected_clone = expected.clone();
    let desc = PartitionDescriptor {
        number: 1,
        partition_guid: None,
        type_guid: None,
        name: None,
        start_lba: 0,
        end_lba: 2047,
        size_bytes: 1024 * 1024,
        flags: 0,
        kind: PartitionKind::BiosBoot,
    };
    let handle = BiosBootHandle::new(desc, move || Ok(expected_clone.clone()));
    assert_eq!(handle.read_raw().unwrap(), expected);
}

/// `RawHandle::read_raw()` returns the bytes provided by the closure.
#[test]
fn test_raw_handle_read_raw() {
    let expected = vec![0xDE, 0xAD, 0xBE, 0xEF];
    let expected_clone = expected.clone();
    let desc = PartitionDescriptor {
        number: 2,
        partition_guid: None,
        type_guid: None,
        name: None,
        start_lba: 2048,
        end_lba: 4095,
        size_bytes: 1024 * 1024,
        flags: 0,
        kind: PartitionKind::Raw,
    };
    let handle = RawHandle::new(desc, move || Ok(expected_clone.clone()));
    assert_eq!(handle.read_raw().unwrap(), expected);
}

/// `BiosBootHandle::read_raw()` propagates errors from the closure.
#[test]
fn test_bios_boot_handle_read_raw_error_propagation() {
    let desc = PartitionDescriptor {
        number: 1,
        partition_guid: None,
        type_guid: None,
        name: None,
        start_lba: 0,
        end_lba: 2047,
        size_bytes: 0,
        flags: 0,
        kind: PartitionKind::BiosBoot,
    };
    let handle = BiosBootHandle::new(desc, || {
        Err(image_delta_core::Error::Format("disk read failure".into()))
    });
    assert!(handle.read_raw().is_err());
}

/// `FsHandle::mount()` propagates errors from the mount closure.
#[test]
fn test_fs_handle_mount_error_propagation() {
    let desc = PartitionDescriptor {
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
    };
    let handle = FsHandle::new(desc, || {
        Err(image_delta_core::Error::Format("nbd connect failed".into()))
    });
    assert!(handle.mount().is_err());
}
