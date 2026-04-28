#![cfg(all(target_os = "linux", feature = "qcow2"))]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{LazyLock, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use nix::mount::{umount2, MntFlags};
use tempfile::TempDir;

use crate::{Image, MountHandle, Result};

// ── constants ─────────────────────────────────────────────────────────────────

/// How long to wait for `/dev/nbdNp1` to appear after `qemu-nbd --connect`.
const NBD_PARTITION_TIMEOUT: Duration = Duration::from_secs(10);
/// Poll interval while waiting for the partition device node.
const NBD_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// How many NBD devices to scan when looking for a free slot.
const NBD_MAX_DEVICES: u32 = 16;

/// Process-global lock serialising `find_free_nbd` + `qemu-nbd --connect`.
///
/// Without this, concurrent calls (e.g. in parallel tests) can both observe
/// the same free device before either has finished connecting, leading to a
/// "connection refused" error on the second caller.  The lock is released as
/// soon as `qemu-nbd --connect` returns successfully, at which point the sysfs
/// `pid` file is populated and later calls to `find_free_nbd` will skip the
/// now-busy device.
static NBD_ALLOC: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

// ── Qcow2MountHandle ──────────────────────────────────────────────────────────

/// RAII handle for a qcow2 image mounted via `qemu-nbd`.
///
/// Dropping this handle will, in order:
/// 1. `umount2(MNT_DETACH)` the mount point
/// 2. `qemu-nbd --disconnect /dev/nbdN` to release the NBD slot
/// 3. Remove the temporary mount directory (via [`TempDir`] drop)
pub struct Qcow2MountHandle {
    /// Temporary directory that serves as the mount point.
    _mount_dir: TempDir,
    /// Cached path returned by `root()`.
    root: PathBuf,
    /// NBD device path, e.g. `/dev/nbd2`.
    nbd_device: String,
}

impl MountHandle for Qcow2MountHandle {
    fn root(&self) -> &Path {
        &self.root
    }
}

impl Drop for Qcow2MountHandle {
    fn drop(&mut self) {
        // 1. Lazy unmount — detach immediately even if busy.
        let _ = umount2(self.root.as_path(), MntFlags::MNT_DETACH);

        // 2. Disconnect the NBD device.
        let _ = Command::new("qemu-nbd")
            .args(["--disconnect", &self.nbd_device])
            .output();

        // 3. TempDir drops last, removing the (now empty) mount point directory.
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Return the path to the first free NBD device at or after `start_index`.
///
/// A device is free when its `/sys/block/nbdN` directory exists but the `pid`
/// file is absent or empty.  Scanning stops at the first index whose sysfs
/// directory does not exist (the kernel only creates entries for devices that
/// have been allocated by the `nbd` module).
///
/// Pass `start_index > 0` (via the `QCOW2_DEVICE` env var) to skip devices
/// that are known to be pre-occupied on the host (e.g. nbd0/nbd1 held by the
/// hypervisor).
///
/// **Must be called while holding [`NBD_ALLOC`]** to prevent a TOCTOU race
/// with concurrent `mount()` calls.
fn find_free_nbd(start_index: u32) -> Result<String> {
    for n in start_index..NBD_MAX_DEVICES {
        let sys_block_dir = format!("/sys/block/nbd{n}");
        if !Path::new(&sys_block_dir).exists() {
            // Kernel has not allocated this device index; stop scanning.
            break;
        }

        let pid_path = format!("{sys_block_dir}/pid");
        match fs::read_to_string(&pid_path) {
            Err(_) => {
                // For idle NBD devices, `pid` may be absent in sysfs.
                return Ok(format!("/dev/nbd{n}"));
            }
            Ok(content) if content.trim().is_empty() => {
                return Ok(format!("/dev/nbd{n}"));
            }
            Ok(_) => {
                // Device is in use, try next.
            }
        }
    }
    Err(crate::Error::Format(format!(
        "no free NBD device found (all /dev/nbd{start_index}..nbd{} are in use)",
        NBD_MAX_DEVICES - 1
    )))
}

/// Connect `qcow2_path` to `nbd_device` via `qemu-nbd --connect=DEVICE`.
///
/// Uses `--read-only` so no write lock is required on the qcow2 file —
/// allowing the same image to be mounted concurrently for inspection.
///
/// The `=` form (`--connect=/dev/nbdN`) is used instead of the space-separated
/// form to avoid qemu-nbd misinterpreting the device path as a server socket.
///
/// **Must be called while holding [`NBD_ALLOC`].**
fn nbd_connect(nbd_device: &str, qcow2_path: &Path) -> Result<()> {
    let path_str = qcow2_path
        .to_str()
        .ok_or_else(|| crate::Error::Format("non-UTF-8 qcow2 path".into()))?;

    let output = Command::new("qemu-nbd")
        .args([
            &format!("--connect={nbd_device}"),
            "--read-only",
            "--detect-zeroes=on",
            path_str,
        ])
        .output()
        .map_err(|e| crate::Error::Format(format!("failed to spawn qemu-nbd: {e}")))?;

    if !output.status.success() {
        return Err(crate::Error::Format(format!(
            "qemu-nbd --connect failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

/// Wait for partition device nodes to appear after `qemu-nbd --connect`, then
/// return an ordered list of block-device candidates to try mounting.
///
/// **Candidate ordering:**
/// - If `QCOW2_PARTITION=N` is set, only `/dev/nbdNpN` is returned (plus the
///   raw device as a fallback).  Use this to skip a known non-FS partition
///   (e.g. a 1 MiB BIOS-boot partition on p1 when the root is on p2).
/// - Otherwise, all `/dev/nbdNp1..p8` that exist are collected in order,
///   followed by the raw device.  The sentinel used to detect readiness is p1
///   (or pN if pinned), since the kernel populates device nodes after reading
///   the whole partition table.
fn wait_for_block_device(nbd_device: &str) -> Vec<String> {
    let pinned: Option<u32> = std::env::var("QCOW2_PARTITION")
        .ok()
        .and_then(|v| v.parse().ok());

    // Wait for the sentinel partition node to appear.
    let sentinel = match pinned {
        Some(n) => format!("{nbd_device}p{n}"),
        None => format!("{nbd_device}p1"),
    };
    let deadline = Instant::now() + NBD_PARTITION_TIMEOUT;
    while Instant::now() < deadline {
        if Path::new(&sentinel).exists() {
            break;
        }
        thread::sleep(NBD_POLL_INTERVAL);
    }

    // Build candidate list.
    match pinned {
        Some(n) => {
            let part = format!("{nbd_device}p{n}");
            if Path::new(&part).exists() {
                vec![part, nbd_device.to_string()]
            } else {
                vec![nbd_device.to_string()]
            }
        }
        None => {
            // Auto-discover: all pN that exist, then the raw device.
            let mut candidates: Vec<String> = (1u32..=8)
                .map(|n| format!("{nbd_device}p{n}"))
                .filter(|p| Path::new(p).exists())
                .collect();
            candidates.push(nbd_device.to_string());
            candidates
        }
    }
}

/// Mount the best available block device from `candidates` at `mount_point`.
///
/// `candidates` is an ordered list produced by [`wait_for_block_device`]:
/// typically `[p1, p2, raw_device]` or `[pN, raw_device]` when
/// `QCOW2_PARTITION` is pinned.
///
/// If `QCOW2_FS` is set, only that filesystem type is tried.  Otherwise
/// ext4, xfs, btrfs, and vfat are attempted in order for each candidate.
///
/// XFS requires `norecovery` for read-only mounts with a potentially dirty
/// journal; without it the kernel returns EINVAL.
fn mount_block_device(candidates: &[String], mount_point: &Path) -> Result<()> {
    use nix::mount::{mount, MsFlags};
    use std::env;

    let flags = MsFlags::MS_RDONLY;
    let pinned_fs: Option<String> = env::var("QCOW2_FS").ok();
    let fstypes: Vec<&str> = match pinned_fs.as_deref() {
        Some(fs) => vec![fs],
        None => vec!["ext4", "xfs", "btrfs", "vfat"],
    };

    let mut last_err_str = String::from("no candidates provided");
    for dev in candidates {
        for fstype in &fstypes {
            let data: Option<&str> = if *fstype == "xfs" {
                Some("norecovery")
            } else {
                None
            };
            match mount(Some(dev.as_str()), mount_point, Some(*fstype), flags, data) {
                Ok(()) => return Ok(()),
                Err(e) => last_err_str = format!("mount({dev}, {fstype}): {e}"),
            }
        }
    }
    Err(crate::Error::Format(format!(
        "mount failed for all candidates/fstypes: {last_err_str}"
    )))
}

// ── Qcow2Image ────────────────────────────────────────────────────────────────

/// [`Image`] implementation for qcow2 VM disk images.
///
/// Requires:
/// - Linux kernel with NBD module (`modprobe nbd` if needed)
/// - `qemu-nbd` in `PATH`
/// - `CAP_SYS_ADMIN` (or equivalent) for `mount(2)` / `umount(2)`
///
/// Feature-gated behind `feature = "qcow2"`.
///
/// # Example (L2 test, requires root/capabilities)
///
/// ```ignore
/// use image_delta_core::{Image, Qcow2Image};
/// let img = Qcow2Image::new();
/// let handle = img.mount(Path::new("base.qcow2")).unwrap();
/// // handle.root() is a Path to the mounted filesystem
/// // dropping `handle` unmounts and disconnects automatically
/// ```
pub struct Qcow2Image;

impl Qcow2Image {
    /// Create a new `Qcow2Image` handler.
    pub fn new() -> Self {
        Self
    }
}

impl Default for Qcow2Image {
    fn default() -> Self {
        Self::new()
    }
}

impl Image for Qcow2Image {
    fn format_name(&self) -> &'static str {
        "qcow2"
    }

    /// Mount `path` (a `.qcow2` file) read-only and return a RAII handle.
    ///
    /// Steps:
    /// 1. Acquire process-global [`NBD_ALLOC`] lock (prevents concurrent TOCTOU)
    /// 2. Find a free `/dev/nbdN` via `/sys/block/nbdN/pid`
    /// 3. `qemu-nbd --connect=/dev/nbdN --read-only <path>`
    /// 4. Release lock (sysfs `pid` is now populated)
    /// 5. Wait up to 10 s for `/dev/nbdNp1` to appear (partition table)
    /// 6. `mount(2)` via `nix` — tries ext4, xfs, btrfs, vfat in order
    /// 7. Return [`Qcow2MountHandle`]; `Drop` handles cleanup
    fn mount(&self, path: &Path) -> Result<Box<dyn MountHandle>> {
        // Allow callers to skip pre-occupied devices (e.g. nbd0/nbd1 held by
        // the hypervisor) by setting QCOW2_DEVICE=N.  Defaults to 0.
        let start_index: u32 = std::env::var("QCOW2_DEVICE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);

        // Serialise device allocation to prevent TOCTOU between concurrent calls.
        let nbd_device = {
            let _guard = NBD_ALLOC
                .lock()
                .map_err(|_| crate::Error::Format("NBD_ALLOC mutex poisoned".into()))?;
            let dev = find_free_nbd(start_index)?;
            nbd_connect(&dev, path)?;
            dev
            // _guard drops here; the device is now visible in sysfs as busy.
        };

        // Wait for partitions to be ready; collect candidates in mount-try order.
        let candidates = wait_for_block_device(&nbd_device);

        // Create a temporary mount point directory.
        let mount_dir = TempDir::new()
            .map_err(|e| crate::Error::Format(format!("failed to create temp dir: {e}")))?;
        let root = mount_dir.path().to_path_buf();

        // Mount the block device. On failure, disconnect NBD before returning.
        if let Err(e) = mount_block_device(&candidates, &root) {
            let _ = Command::new("qemu-nbd")
                .args(["--disconnect", &nbd_device])
                .output();
            return Err(e);
        }

        Ok(Box::new(Qcow2MountHandle {
            _mount_dir: mount_dir,
            root,
            nbd_device,
        }))
    }

    /// Pack is implemented in Phase 5.3.
    fn pack(&self, _source_dir: &Path, _output_path: &Path) -> Result<()> {
        todo!("Phase 5.3: create qcow2 image from source_dir via qemu-img convert")
    }
}
