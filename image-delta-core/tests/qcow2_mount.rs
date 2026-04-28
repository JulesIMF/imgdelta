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
}
