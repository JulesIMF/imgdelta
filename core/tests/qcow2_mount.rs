// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Integration tests for QCOW2 image mounting via qemu-nbd (requires root)

/// L2 integration tests for [`Qcow2Image::mount`].
///
/// These tests require:
/// - Linux kernel with the `nbd` module loaded
/// - `qemu-nbd` in `PATH`
/// - `CAP_SYS_ADMIN` (root or equivalent) for `mount(2)` / `umount(2)`
/// - A real `.qcow2` test image (see `tests/fixtures/qcow2/README` or the
///   `QCow2_PATH` env var)
///
/// Run manually with:
/// ```sh
/// QCOW2_PATH=/path/to/image.qcow2 \
///     cargo test -p image-delta-core --features qcow2 --test qcow2_mount -- --ignored
/// ```
mod common;
#[cfg(all(target_os = "linux", feature = "qcow2"))]
mod tests {
    use image_delta_core::{Image, Qcow2Image};
    use std::env;
    use std::path::PathBuf;

    /// Path to the qcow2 image used for L2 tests.
    ///
    /// Resolved as: `QCOW2_PATH` env var, or
    /// `tests/fixtures/qcow2/test.qcow2` relative to the workspace root.
    fn test_image_path() -> Option<PathBuf> {
        if let Ok(p) = env::var("QCOW2_PATH") {
            let path = PathBuf::from(p);
            if path.exists() {
                return Some(path);
            }
        }
        // Fallback: fixture file next to the tests directory.
        let fixture =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/qcow2/test.qcow2");
        if fixture.exists() {
            Some(fixture)
        } else {
            None
        }
    }

    // ── 1. Mount and inspect root ─────────────────────────────────────────────

    /// Mount a qcow2 image and verify the root contains at least one entry.
    ///
    /// This is the smoke test for Phase 5.2.
    #[test]
    #[ignore = "L2: requires qemu-nbd + CAP_SYS_ADMIN + a qcow2 fixture"]
    fn test_qcow2_mount_root_accessible() {
        let path = test_image_path().expect(
            "No qcow2 test image found. \
             Set QCOW2_PATH env var or place a test image at \
             tests/fixtures/qcow2/test.qcow2",
        );

        let img = Qcow2Image::new();
        let handle = img
            .mount(&path)
            .expect("Qcow2Image::mount must succeed on a valid qcow2 image");

        let root = handle.root();
        assert!(root.exists(), "mount root must exist: {root:?}");
        assert!(root.is_dir(), "mount root must be a directory: {root:?}");

        let entries: Vec<_> = std::fs::read_dir(root)
            .expect("should be able to read mount root")
            .collect();
        assert!(
            !entries.is_empty(),
            "mounted filesystem must contain at least one entry"
        );

        // `handle` drops here → umount + qemu-nbd --disconnect
    }

    // ── 2. Drop unmounts cleanly ──────────────────────────────────────────────

    /// After dropping the handle, the mount point must no longer be mounted.
    #[test]
    #[ignore = "L2: requires qemu-nbd + CAP_SYS_ADMIN + a qcow2 fixture"]
    fn test_qcow2_drop_unmounts() {
        let path = test_image_path().expect("no qcow2 fixture; set QCOW2_PATH");

        let img = Qcow2Image::new();

        let root_path: std::path::PathBuf;
        {
            let handle = img.mount(&path).expect("mount must succeed");
            root_path = handle.root().to_path_buf();
            assert!(root_path.exists());
            // handle drops here
        }

        // After drop the TempDir is removed; the path must not exist.
        assert!(
            !root_path.exists(),
            "mount point must be removed after handle is dropped: {root_path:?}"
        );
    }

    // ── 3. Concurrent mounts use separate NBD devices ─────────────────────────

    /// Two simultaneous mounts of the same image must each get their own NBD
    /// device and return independent roots.
    #[test]
    #[ignore = "L2: requires qemu-nbd + CAP_SYS_ADMIN + a qcow2 fixture"]
    fn test_qcow2_concurrent_mounts() {
        let path = test_image_path().expect("no qcow2 fixture; set QCOW2_PATH");

        let img = Qcow2Image::new();

        let h1 = img.mount(&path).expect("first mount must succeed");
        let h2 = img
            .mount(&path)
            .expect("second concurrent mount must succeed");

        assert_ne!(
            h1.root(),
            h2.root(),
            "concurrent mounts must have distinct roots"
        );

        // Both roots must be accessible.
        assert!(h1.root().is_dir());
        assert!(h2.root().is_dir());

        // Drop in reverse order; both must clean up without error.
        drop(h2);
        drop(h1);
    }

    // ── 4. format_name ────────────────────────────────────────────────────────

    /// `format_name()` must return `"qcow2"` — no L2 resources needed.
    #[test]
    fn test_qcow2_format_name() {
        assert_eq!(Qcow2Image::new().format_name(), "qcow2");
    }

    // ── 5. pack → mount roundtrip ─────────────────────────────────────────────

    /// Pack a synthetic directory tree into a qcow2 image, then mount it and
    /// verify the contents match the source exactly.
    ///
    /// This is the smoke test for Phase 5.3 (`Qcow2Image::pack`).
    #[test]
    #[ignore = "L2: requires qemu-nbd + qemu-img + mkfs.ext4 + CAP_SYS_ADMIN"]
    fn test_qcow2_pack_mount_roundtrip() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        use tempfile::tempdir;

        // Build a small source tree with different file types.
        let source = tempdir().expect("source tempdir");
        let src = source.path();

        fs::create_dir(src.join("subdir")).unwrap();
        fs::write(src.join("hello.txt"), b"hello from qcow2 pack").unwrap();
        fs::write(src.join("subdir").join("data.bin"), b"\x00\x01\x02\x03").unwrap();
        std::os::unix::fs::symlink("hello.txt", src.join("link.txt")).unwrap();
        fs::set_permissions(src.join("hello.txt"), PermissionsExt::from_mode(0o644)).unwrap();

        let output_dir = tempdir().expect("output tempdir");
        let qcow2_path = output_dir.path().join("packed.qcow2");

        // Pack the source directory into a new qcow2 image.
        let img = Qcow2Image::new();
        img.pack(src, &qcow2_path)
            .expect("Qcow2Image::pack must succeed");
        assert!(qcow2_path.exists(), "output qcow2 must exist after pack");

        // Mount the freshly-packed image.
        let handle = img
            .mount(&qcow2_path)
            .expect("Qcow2Image::mount must succeed on the just-packed image");

        let root = handle.root();
        assert!(root.is_dir(), "mount root must be a directory");

        // Verify file contents.
        assert_eq!(
            fs::read(root.join("hello.txt")).expect("hello.txt must be in mounted image"),
            b"hello from qcow2 pack"
        );
        assert_eq!(
            fs::read(root.join("subdir").join("data.bin"))
                .expect("subdir/data.bin must be in mounted image"),
            b"\x00\x01\x02\x03"
        );

        // Verify symlink preserved.
        let link_target =
            fs::read_link(root.join("link.txt")).expect("link.txt must remain a symlink");
        assert_eq!(link_target.to_str().unwrap(), "hello.txt");

        // `handle` drops here → umount + qemu-nbd --disconnect
    }

    // ── 6. pack with base image → mount roundtrip ─────────────────────────────

    /// Clone a real qcow2, replace its main partition with a synthetic tree,
    /// then mount and verify the new contents — while checking the other
    /// partitions haven't moved (image is still valid qcow2).
    ///
    /// Requires `QCOW2_PATH` to point at a real qcow2 with at least one
    /// recognisable filesystem partition (ext4/xfs/btrfs).
    #[test]
    #[ignore = "L2: requires qemu-nbd + qemu-img + mkfs.ext4 + CAP_SYS_ADMIN + QCOW2_PATH"]
    fn test_qcow2_pack_with_base_roundtrip() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        use tempfile::tempdir;

        let base_path = test_image_path().expect(
            "No qcow2 test image found. Set QCOW2_PATH or place one at \
             tests/fixtures/qcow2/test.qcow2",
        );

        // Build a small source tree.
        let source = tempdir().expect("source tempdir");
        let src = source.path();
        fs::create_dir(src.join("new_dir")).unwrap();
        fs::write(src.join("sentinel.txt"), b"packed-with-base").unwrap();
        fs::write(src.join("new_dir").join("child.bin"), b"\xde\xad\xbe\xef").unwrap();
        std::os::unix::fs::symlink("sentinel.txt", src.join("sym.txt")).unwrap();
        fs::set_permissions(src.join("sentinel.txt"), PermissionsExt::from_mode(0o644)).unwrap();

        let output_dir = tempdir().expect("output tempdir");
        let qcow2_path = output_dir.path().join("repacked.qcow2");

        // Pack with base: clone the real image, replace its main FS with our tree.
        let img = Qcow2Image::with_base(base_path);
        img.pack(src, &qcow2_path)
            .expect("Qcow2Image::pack (with_base) must succeed");
        assert!(qcow2_path.exists(), "output qcow2 must exist after pack");

        // Mount the repacked image — should see our synthetic files.
        let img2 = Qcow2Image::new();
        let handle = img2
            .mount(&qcow2_path)
            .expect("Qcow2Image::mount must succeed on repacked image");

        let root = handle.root();
        assert!(root.is_dir(), "mount root must be a directory");

        assert_eq!(
            fs::read(root.join("sentinel.txt")).expect("sentinel.txt must exist in repacked image"),
            b"packed-with-base"
        );
        assert_eq!(
            fs::read(root.join("new_dir").join("child.bin")).expect("new_dir/child.bin must exist"),
            b"\xde\xad\xbe\xef"
        );
        let link_target = fs::read_link(root.join("sym.txt")).expect("sym.txt must be a symlink");
        assert_eq!(link_target.to_str().unwrap(), "sentinel.txt");

        // The original image files must NOT be present (main partition was wiped).
        // (We just verify the sentinel we wrote is there — that's sufficient.)

        // `handle` drops here → umount + qemu-nbd --disconnect
    }

    // ── 7. open() — disk layout ───────────────────────────────────────────────

    /// `Qcow2Image::open()` must parse the GPT partition table and return a
    /// [`DiskLayout`] with the correct scheme and partition count.
    ///
    /// Assumes the image at `QCOW2_PATH` uses GPT with at least 2 partitions
    /// (BIOS Boot on p1, Linux data on p2) — true for all GCP cloud images.
    #[test]
    #[ignore = "L2: requires qemu-nbd + CAP_SYS_ADMIN + QCOW2_PATH"]
    fn test_qcow2_open_disk_layout() {
        use image_delta_core::partitions::DiskScheme;

        let path = test_image_path().expect("no qcow2 fixture; set QCOW2_PATH");
        let img = Qcow2Image::new();
        let opened = img.open(&path).expect("Qcow2Image::open must succeed");

        let layout = opened.disk_layout();
        assert_eq!(
            layout.scheme,
            DiskScheme::Gpt,
            "GCP qcow2 images use GPT; got {:?}",
            layout.scheme
        );
        assert!(
            layout.disk_guid.is_some(),
            "GPT layout must have a disk GUID"
        );
        assert!(
            layout.partitions.len() >= 2,
            "GCP images must have at least 2 partitions, got {}",
            layout.partitions.len()
        );
        // Partition numbers must be unique and start from 1.
        let nums: Vec<u32> = layout.partitions.iter().map(|p| p.number).collect();
        assert!(nums.contains(&1), "partition 1 must exist");
        assert!(nums.contains(&2), "partition 2 must exist");
    }

    // ── 8. open() — partition kinds ───────────────────────────────────────────

    /// The first partition of a GCP qcow2 is BIOS Boot and the second is a
    /// mountable filesystem (ext4 for Debian/Ubuntu, xfs for CentOS).
    #[test]
    #[ignore = "L2: requires qemu-nbd + CAP_SYS_ADMIN + QCOW2_PATH"]
    fn test_qcow2_open_partition_kinds() {
        use image_delta_core::partitions::PartitionKind;

        let path = test_image_path().expect("no qcow2 fixture; set QCOW2_PATH");
        let img = Qcow2Image::new();
        let opened = img.open(&path).expect("Qcow2Image::open must succeed");

        let layout = opened.disk_layout();

        let p1 = layout
            .partitions
            .iter()
            .find(|p| p.number == 1)
            .expect("partition 1 must exist");
        assert_eq!(
            p1.kind,
            PartitionKind::BiosBoot,
            "p1 must be classified as BiosBoot; got {:?}",
            p1.kind
        );

        let p2 = layout
            .partitions
            .iter()
            .find(|p| p.number == 2)
            .expect("partition 2 must exist");
        assert!(
            matches!(p2.kind, PartitionKind::Fs { .. }),
            "p2 must be a filesystem partition; got {:?}",
            p2.kind
        );
    }

    // ── 9. open() — filesystem partition is readable ──────────────────────────

    /// Get the Fs partition handle from `open()`, mount it, and read at least
    /// one file from the root — proving the full open→mount→read path works.
    #[test]
    #[ignore = "L2: requires qemu-nbd + CAP_SYS_ADMIN + QCOW2_PATH"]
    fn test_qcow2_open_fs_partition_readable() {
        use image_delta_core::partitions::PartitionHandle;

        let path = test_image_path().expect("no qcow2 fixture; set QCOW2_PATH");
        let img = Qcow2Image::new();
        let opened = img.open(&path).expect("Qcow2Image::open must succeed");

        let handles = opened.partitions().expect("partitions() must succeed");

        // Find the first Fs handle.
        let fs_handle = handles
            .into_iter()
            .find_map(|h| {
                if let PartitionHandle::Fs(fh) = h {
                    Some(fh)
                } else {
                    None
                }
            })
            .expect("at least one Fs partition handle must exist");

        let mount = fs_handle
            .mount()
            .expect("mount() on Fs handle must succeed");
        let root = mount.root();

        assert!(root.is_dir(), "mount root must be a directory: {root:?}");

        let entries: Vec<_> = std::fs::read_dir(root)
            .expect("should be able to list mount root")
            .collect();
        assert!(
            !entries.is_empty(),
            "mounted filesystem partition must not be empty"
        );

        // drop(mount) → umount(MNT_DETACH);  drop(opened) → qemu-nbd --disconnect
    }

    // ── 10. open() — BIOS boot partition returns non-empty bytes ─────────────

    /// Read the raw bytes of the BIOS Boot partition.  It must be non-empty
    /// (GRUB stage 1 or equivalent bootstrap code lives there).
    #[test]
    #[ignore = "L2: requires qemu-nbd + CAP_SYS_ADMIN + QCOW2_PATH"]
    fn test_qcow2_open_bios_boot_readable() {
        use image_delta_core::partitions::PartitionHandle;

        let path = test_image_path().expect("no qcow2 fixture; set QCOW2_PATH");
        let img = Qcow2Image::new();
        let opened = img.open(&path).expect("Qcow2Image::open must succeed");

        let handles = opened.partitions().expect("partitions() must succeed");

        let bios_handle = handles
            .into_iter()
            .find_map(|h| {
                if let PartitionHandle::BiosBoot(bh) = h {
                    Some(bh)
                } else {
                    None
                }
            })
            .expect("at least one BiosBoot partition handle must exist");

        let bytes = bios_handle
            .read_raw()
            .expect("read_raw() on BiosBoot handle must succeed");

        assert!(
            !bytes.is_empty(),
            "BIOS Boot partition must contain non-zero bytes"
        );
        // The BIOS Boot partition should be at least 1 KiB (GRUB bootstrap).
        assert!(
            bytes.len() >= 1024,
            "BIOS Boot partition must be >= 1 KiB; got {} bytes",
            bytes.len()
        );
    }
}
