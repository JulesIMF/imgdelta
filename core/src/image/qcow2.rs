// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Qcow2Image: opens QCOW2 disk images via NBD / qemu-nbd (Phase 6.F)

#![cfg(all(target_os = "linux", feature = "qcow2"))]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, LazyLock, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use nix::mount::{umount2, MntFlags};
use tempfile::TempDir;
use walkdir::WalkDir;

use crate::image::OpenImage;
use crate::manifest::{PartitionContent, PartitionManifest};
use crate::partitions::{BiosBootHandle, FsHandle, MbrHandle, PartitionHandle, RawHandle};
use crate::partitions::{DiskLayout, DiskScheme, PartitionDescriptor, PartitionKind};
use crate::{Image, MountHandle, Result};
use tracing::debug;

// ── constants ─────────────────────────────────────────────────────────────────

/// How long to wait for `/dev/nbdNp1` to appear after `qemu-nbd --connect`.
const NBD_PARTITION_TIMEOUT: Duration = Duration::from_secs(10);
/// Poll interval while waiting for the partition device node.
const NBD_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// How many NBD devices to scan when looking for a free slot.
const NBD_MAX_DEVICES: u32 = 16;
/// Logical sector size assumed for all disk offset calculations.
const SECTOR_SIZE: u64 = 512;

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

        // 3. Wait for the kernel to release the NBD slot (size → 0 in sysfs)
        //    so that subsequent calls to find_free_nbd() don't see it as busy.
        wait_for_nbd_disconnected(&self.nbd_device);

        // 4. TempDir drops last, removing the (now empty) mount point directory.
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Wait until `/sys/block/nbdN/size` becomes non-zero, indicating that the
/// kernel has fully connected the NBD device to a qemu-nbd server.
///
/// **Call this while holding [`NBD_ALLOC`]** so that concurrent `open()` /
/// `mount()` calls see the device as busy and skip to the next one.
///
/// The sysfs `pid` attribute is NOT used because it records a transient
/// fork-helper PID that disappears as soon as the daemon child continues —
/// making it unreliable for liveness checks.
fn wait_for_nbd_connected(dev: &str) {
    let base_name = dev.trim_start_matches("/dev/"); // e.g. "nbd0"
    let size_path = format!("/sys/block/{base_name}/size");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if Instant::now() >= deadline {
            break;
        }
        let size: u64 = fs::read_to_string(&size_path)
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        if size > 0 {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// Poll `/sys/block/nbdN/size` until it returns to 0, confirming that the
/// kernel has released the NBD slot after `qemu-nbd --disconnect`.
///
/// Without this, a subsequent `find_free_nbd` call may see the device as
/// still in use and skip to the next slot, eventually exhausting all 16 NBD
/// devices across multiple roundtrip runs.
fn wait_for_nbd_disconnected(dev: &str) {
    let base_name = dev.trim_start_matches("/dev/");
    let size_path = format!("/sys/block/{base_name}/size");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if Instant::now() >= deadline {
            break;
        }
        let size: u64 = fs::read_to_string(&size_path)
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        if size == 0 {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// Wait until `/dev/nbdNpM` exists **and** has remained present continuously
/// for at least `STABILITY_WINDOW`.
///
/// After `sgdisk --clear` / `partprobe` the kernel re-reads the partition
/// table asynchronously and may create, delete, and re-create the partition
/// device nodes several times within a few hundred milliseconds.  Waiting only
/// for the first appearance is insufficient: `mkfs` or `blkid` issued
/// immediately after may find the node temporarily absent and fail with
/// "does not exist and no size was specified".
fn wait_for_partition_stable(
    nbd_device: &str,
    part_num: u32,
    timeout: Duration,
) -> crate::Result<String> {
    /// How long the node must be present without interruption.
    const STABILITY_WINDOW: Duration = Duration::from_millis(400);
    const POLL: Duration = Duration::from_millis(50);

    let part_dev = format!("{nbd_device}p{part_num}");
    let deadline = Instant::now() + timeout;
    let mut stable_since: Option<Instant> = None;

    while Instant::now() < deadline {
        if Path::new(&part_dev).exists() {
            match stable_since {
                None => {
                    // Node appeared for the first time — start the stability timer.
                    stable_since = Some(Instant::now());
                }
                Some(t) if t.elapsed() >= STABILITY_WINDOW => {
                    // Node has been present long enough; it is stable.
                    return Ok(part_dev);
                }
                _ => {
                    // Still within the stability window; keep polling.
                }
            }
        } else {
            // Node disappeared (kernel re-reading partition table); reset timer.
            stable_since = None;
        }
        thread::sleep(POLL);
    }

    Err(crate::Error::Format(format!(
        "timeout waiting for {part_dev} to become stable ({}s)",
        timeout.as_secs()
    )))
}

/// Return the path to the first free NBD device at or after `start_index`.
///
/// A device is free when its `/sys/block/nbdN/size` reads `0`.  Scanning stops
/// at the first index whose sysfs directory does not exist (the kernel only
/// creates entries for devices that have been allocated by the `nbd` module).
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

        let size_path = format!("{sys_block_dir}/size");
        let size: u64 = fs::read_to_string(&size_path)
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);

        if size == 0 {
            // Device is free (size == 0 means not connected to any image).
            return Ok(format!("/dev/nbd{n}"));
        }

        // size > 0 → device is in use by another connection.
        // (The pid file is unreliable: it may point to a transient fork-helper
        //  PID that no longer exists even though the NBD daemon is alive.)
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
            // Both ext4 and XFS need `norecovery` when mounted read-only with a
            // dirty journal: without it the kernel tries to replay the journal but
            // cannot write, returning EROFS ("Read-only file system").
            // XFS also needs `nouuid` to allow mounting multiple XFS filesystems
            // with the same UUID (base and target partitions share UUID).
            let data: Option<&str> = if *fstype == "xfs" {
                Some("norecovery,nouuid")
            } else if *fstype == "ext4" {
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

// ── pack helpers ──────────────────────────────────────────────────────────────

/// Sum the byte sizes of all regular files under `dir`.
fn dir_total_size(dir: &Path) -> Result<u64> {
    let mut total = 0u64;
    for entry in WalkDir::new(dir) {
        let entry = entry.map_err(|e| crate::Error::Format(e.to_string()))?;
        if entry.file_type().is_file() {
            total += entry
                .metadata()
                .map_err(|e| crate::Error::Format(e.to_string()))?
                .len();
        }
    }
    Ok(total)
}

/// Calculate a qcow2/ext4 image size that comfortably fits `data_bytes` of
/// content.  Adds 50% overhead for ext4 metadata and journal, with a minimum
/// of 64 MiB (the smallest ext4 image `mkfs.ext4` accepts without `-F`).
fn ext4_image_size(data_bytes: u64) -> u64 {
    const MIN: u64 = 64 * 1024 * 1024; // 64 MiB
    let padded = (data_bytes * 3 / 2).max(MIN);
    // Round up to next 1 MiB boundary.
    (padded + (1024 * 1024 - 1)) & !(1024 * 1024 - 1)
}

/// Use `blkid` to find the first partition on `nbd_device` that contains a
/// recognised filesystem (ext4 / xfs / btrfs / vfat).
///
/// Falls back to the `QCOW2_PARTITION` env var when set.  Returns the full
/// device path (e.g. `/dev/nbd2p2`) or an error if nothing is found.
fn find_main_partition(nbd_device: &str) -> Result<String> {
    // Env-var override: QCOW2_PARTITION=N
    if let Some(n) = std::env::var("QCOW2_PARTITION")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
    {
        return Ok(format!("{nbd_device}p{n}"));
    }

    // Auto-detect via blkid: pick the first partition with a real FS.
    for n in 1u32..=8 {
        let part = format!("{nbd_device}p{n}");
        if !Path::new(&part).exists() {
            continue;
        }
        if let Ok(out) = Command::new("blkid").arg(&part).output() {
            let s = String::from_utf8_lossy(&out.stdout);
            if s.contains("ext4") || s.contains("xfs") || s.contains("btrfs") || s.contains("vfat")
            {
                return Ok(part);
            }
        }
    }

    Err(crate::Error::Format(format!(
        "find_main_partition: no recognisable FS found on any partition of {nbd_device}"
    )))
}

/// Connect `qcow2_path` to `nbd_device` for writing (no `--read-only`).
///
/// **Must be called while holding [`NBD_ALLOC`].**
fn nbd_connect_rw(nbd_device: &str, qcow2_path: &Path) -> Result<()> {
    let path_str = qcow2_path
        .to_str()
        .ok_or_else(|| crate::Error::Format("non-UTF-8 qcow2 path".into()))?;
    run_command(
        Command::new("qemu-nbd").args([&format!("--connect={nbd_device}"), path_str]),
        "qemu-nbd --connect (rw)",
    )
}

/// Mount `block_device` read-write at `mount_point` trying ext4.
fn mount_rw(block_device: &str, mount_point: &Path) -> Result<()> {
    use nix::mount::{mount, MsFlags};
    mount(
        Some(block_device),
        mount_point,
        Some("ext4"),
        MsFlags::empty(),
        None::<&str>,
    )
    .map_err(|e| crate::Error::Format(format!("mount_rw({block_device}): {e}")))
}

/// Run a `Command`, returning an error if the process fails.
fn run_command(cmd: &mut Command, label: &str) -> Result<()> {
    let out = cmd
        .output()
        .map_err(|e| crate::Error::Format(format!("failed to spawn {label}: {e}")))?;
    if !out.status.success() {
        return Err(crate::Error::Format(format!(
            "{label} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

/// Core of `pack()` when no base image is provided.
///
/// Creates a fresh qcow2 with a single raw ext4 partition (no partition table).
/// Suitable for tests.
fn pack_fresh(source_dir: &Path, output_path: &Path) -> Result<()> {
    let data_bytes = dir_total_size(source_dir)?;
    let image_size_str = format!("{}", ext4_image_size(data_bytes));

    run_command(
        Command::new("qemu-img").args([
            "create",
            "-f",
            "qcow2",
            output_path
                .to_str()
                .ok_or_else(|| crate::Error::Format("non-UTF-8 output path".into()))?,
            &image_size_str,
        ]),
        "qemu-img create",
    )?;

    let start_index: u32 = std::env::var("QCOW2_DEVICE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let nbd_device = {
        let _guard = NBD_ALLOC
            .lock()
            .map_err(|_| crate::Error::Format("NBD_ALLOC mutex poisoned".into()))?;
        let dev = find_free_nbd(start_index)?;
        nbd_connect_rw(&dev, output_path)?;
        wait_for_nbd_connected(&dev);
        dev
    };

    let disconnect = || {
        let _ = Command::new("qemu-nbd")
            .args(["--disconnect", &nbd_device])
            .output();
    };

    if let Err(e) = run_command(
        Command::new("mkfs.ext4").args(["-F", &nbd_device]),
        "mkfs.ext4",
    ) {
        disconnect();
        return Err(e);
    }

    // Use the shared copy helper.
    let result = copy_into_nbd(source_dir, &nbd_device, mount_rw);
    disconnect();
    result
}

/// Core of `pack()` when a base image is provided.
///
/// 1. Clone the base qcow2 (sparse copy, preserves partition table + all partitions)
/// 2. Connect the clone via qemu-nbd (writable)
/// 3. Auto-detect the main FS partition via `blkid`
/// 4. `mkfs.ext4 -F <partition>` — wipe and reformat only that partition
/// 5. Mount read-write, copy tree, unmount
/// 6. Disconnect
fn pack_with_base(base: &Path, source_dir: &Path, output_path: &Path) -> Result<()> {
    // 1. Sparse-copy the base image.
    run_command(
        Command::new("cp").args([
            "--sparse=always",
            base.to_str()
                .ok_or_else(|| crate::Error::Format("non-UTF-8 base path".into()))?,
            output_path
                .to_str()
                .ok_or_else(|| crate::Error::Format("non-UTF-8 output path".into()))?,
        ]),
        "cp --sparse=always (clone base)",
    )?;

    // 2. Connect the clone via NBD (writable).
    let start_index: u32 = std::env::var("QCOW2_DEVICE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let nbd_device = {
        let _guard = NBD_ALLOC
            .lock()
            .map_err(|_| crate::Error::Format("NBD_ALLOC mutex poisoned".into()))?;
        let dev = find_free_nbd(start_index)?;
        nbd_connect_rw(&dev, output_path)?;
        wait_for_nbd_connected(&dev);
        dev
    };

    let disconnect = || {
        let _ = Command::new("qemu-nbd")
            .args(["--disconnect", &nbd_device])
            .output();
    };

    // Wait for the main filesystem partition node to become stable before
    // probing with blkid.  The partition table re-read is asynchronous and
    // may cause p1/p2 to appear and disappear several times.
    // We wait for p1 (sentinel) first, then individually for each candidate.
    let _ = wait_for_block_device(&nbd_device);
    // Additionally ensure the specific partition nodes used by blkid are stable.
    for n in 1u32..=4 {
        let _ = wait_for_partition_stable(&nbd_device, n, Duration::from_secs(8));
    }

    // 3. Find the main filesystem partition.
    let main_part = match find_main_partition(&nbd_device) {
        Ok(p) => p,
        Err(e) => {
            disconnect();
            return Err(e);
        }
    };

    // 4. Wipe and reformat just that partition.
    if let Err(e) = run_command(
        Command::new("mkfs.ext4").args(["-F", &main_part]),
        "mkfs.ext4 (main partition)",
    ) {
        disconnect();
        return Err(e);
    }

    // 5. Mount read-write and copy tree.
    let result = copy_into_nbd(source_dir, &main_part, mount_rw);

    // 6. Disconnect.
    disconnect();
    result
}

/// Mount `nbd_device_or_partition` read-write, copy `source_dir` into it, then unmount.
fn copy_into_nbd<F>(source_dir: &Path, device: &str, mount_fn: F) -> Result<()>
where
    F: FnOnce(&str, &Path) -> Result<()>,
{
    let mount_dir = TempDir::new()
        .map_err(|e| crate::Error::Format(format!("failed to create temp dir: {e}")))?;
    let mount_root = mount_dir.path();

    mount_fn(device, mount_root)?;

    let src_str = source_dir
        .to_str()
        .ok_or_else(|| crate::Error::Format("non-UTF-8 source path".into()))?;
    let dst_str = mount_root
        .to_str()
        .ok_or_else(|| crate::Error::Format("non-UTF-8 mount root path".into()))?;

    let cp_src = format!("{src_str}/.");
    let result = run_command(Command::new("cp").args(["-a", &cp_src, dst_str]), "cp -a");

    let _ = umount2(mount_root, MntFlags::MNT_DETACH);
    result
}

// ── sfdisk JSON parsing ───────────────────────────────────────────────────────

/// Top-level wrapper produced by `sfdisk --json`.
#[derive(serde::Deserialize)]
struct SfdiskOutput {
    partitiontable: SfdiskTable,
}

#[derive(serde::Deserialize)]
struct SfdiskTable {
    label: String,
    #[serde(default)]
    id: Option<String>,
    sectorsize: u64,
    partitions: Vec<SfdiskPartition>,
}

#[derive(serde::Deserialize)]
struct SfdiskPartition {
    /// Full block-device node, e.g. `/dev/nbd3p2`.
    node: String,
    /// First sector (LBA).
    start: u64,
    /// Partition size in sectors.
    size: u64,
    /// GPT type GUID string or MBR type number.
    #[serde(rename = "type", default)]
    part_type: Option<String>,
    /// GPT partition GUID string.
    #[serde(default)]
    uuid: Option<String>,
    /// Partition label (UTF-8, decoded from GPT UTF-16).
    #[serde(default)]
    name: Option<String>,
}

// ── GPT type GUID constants ───────────────────────────────────────────────────

/// BIOS Boot partition (`21686148-6449-6E6F-744E-656564454649`).
const GUID_BIOS_BOOT: &str = "21686148-6449-6e6f-744e-656564454649";
/// EFI System partition (`C12A7328-F81F-11D2-BA4B-00A0C93EC93B`).
const GUID_EFI_SYSTEM: &str = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b";
/// Linux Swap partition (`0657FD6D-A4AB-43C4-84E5-0933C84B4F4F`).
const GUID_LINUX_SWAP: &str = "0657fd6d-a4ab-43c4-84e5-0933c84b4f4f";

// ── NbdConn — RAII disconnect-only wrapper ────────────────────────────────────

/// RAII wrapper around an open NBD connection.
///
/// `Drop` calls `qemu-nbd --disconnect` to release the kernel NBD slot.
/// Does **not** unmount any filesystems that may be mounted on partitions
/// of this device — that is the responsibility of each [`PartitionMountHandle`].
struct NbdConn(String);

impl Drop for NbdConn {
    fn drop(&mut self) {
        let _ = Command::new("qemu-nbd")
            .args(["--disconnect", &self.0])
            .output();
        // Wait until the kernel marks the device as free (size=0 in sysfs).
        // Without this, a rapid subsequent find_free_nbd() may still see the
        // device as in-use and skip it, eventually exhausting all NBD slots.
        wait_for_nbd_disconnected(&self.0);
    }
}

// ── PartitionMountHandle ──────────────────────────────────────────────────────

/// RAII handle for a single partition mount within an open qcow2.
///
/// `Drop` calls `umount2(MNT_DETACH)` but does **not** disconnect the NBD
/// device.  The `Arc<NbdConn>` field ensures the NBD connection stays alive
/// until every partition handle derived from the same [`OpenQcow2Image`] is
/// dropped.
struct PartitionMountHandle {
    _mount_dir: TempDir,
    root: PathBuf,
    /// Keeps the shared NBD connection alive.
    _nbd: Arc<NbdConn>,
}

impl MountHandle for PartitionMountHandle {
    fn root(&self) -> &Path {
        &self.root
    }
}

impl Drop for PartitionMountHandle {
    fn drop(&mut self) {
        let _ = umount2(self.root.as_path(), MntFlags::MNT_DETACH);
        // _nbd drops here if this is the last Arc; qemu-nbd --disconnect
        // is called only when all partition handles for this image are gone.
    }
}

// ── RwOutputMountHandle ───────────────────────────────────────────────────────

/// RAII handle to an RW-mounted partition directory used during decompress output.
///
/// Unlike [`PartitionMountHandle`] this is not tied to a shared [`NbdConn`]:
/// the output NBD connection is kept alive by the `_out_nbd` guard in the
/// caller's scope.  On drop it simply calls `umount2(MNT_DETACH)`.
struct RwOutputMountHandle {
    _dir: TempDir,
    root: PathBuf,
}

impl MountHandle for RwOutputMountHandle {
    fn root(&self) -> &Path {
        &self.root
    }
}

impl Drop for RwOutputMountHandle {
    fn drop(&mut self) {
        let _ = umount2(self.root.as_path(), MntFlags::MNT_DETACH);
    }
}

// ── PartInfo ──────────────────────────────────────────────────────────────────

/// Precomputed info for a single partition in an open qcow2 image.
struct PartInfo {
    desc: PartitionDescriptor,
    /// Block-device path, e.g. `/dev/nbd3p2`.
    block_dev: String,
}

// ── OpenQcow2Image ────────────────────────────────────────────────────────────

/// [`OpenImage`] implementation for a qcow2 image opened via NBD.
///
/// Holds an `Arc<NbdConn>` so the NBD connection is not released until this
/// object **and** all [`PartitionHandle`]s derived from it are dropped.
struct OpenQcow2Image {
    layout: DiskLayout,
    part_infos: Vec<PartInfo>,
    nbd: Arc<NbdConn>,
    /// First 440 bytes of the raw disk (MBR boot-code area), captured at
    /// `open()` time.  `None` when the read failed (non-fatal).
    mbr_bytes: Option<Arc<Vec<u8>>>,
}

impl OpenImage for OpenQcow2Image {
    fn disk_layout(&self) -> &DiskLayout {
        &self.layout
    }

    fn partitions(&self) -> crate::Result<Vec<PartitionHandle>> {
        let mut handles = Vec::new();

        // Prepend the synthetic MBR boot-code handle (number 0) whenever we
        // successfully read those bytes at open time.  The compressor will
        // store them as PartitionContent::MbrBootCode so the decompressor can
        // restore them after sgdisk --clear rewrites LBA 0.
        if let Some(bytes) = &self.mbr_bytes {
            let bytes = Arc::clone(bytes);
            let desc = PartitionDescriptor {
                number: 0,
                partition_guid: None,
                type_guid: None,
                name: Some("MBR boot code".into()),
                start_lba: 0,
                end_lba: 0,
                size_bytes: 440,
                flags: 0,
                kind: PartitionKind::MbrBootCode,
            };
            handles.push(PartitionHandle::Mbr(MbrHandle::new(desc, move || {
                Ok((*bytes).clone())
            })));
        }

        for pi in &self.part_infos {
            let nbd = Arc::clone(&self.nbd);
            let dev = pi.block_dev.clone();
            let desc = pi.desc.clone();
            let kind = desc.kind.clone();

            let handle = match kind {
                PartitionKind::BiosBoot => {
                    let size = desc.size_bytes as usize;
                    PartitionHandle::BiosBoot(BiosBootHandle::new(desc, move || {
                        let _ = &nbd; // keep NbdConn alive while reading
                        read_block_device_bytes(&dev, size)
                    }))
                }
                PartitionKind::Raw => {
                    let size = desc.size_bytes as usize;
                    PartitionHandle::Raw(RawHandle::new(desc, move || {
                        let _ = &nbd;
                        read_block_device_bytes(&dev, size)
                    }))
                }
                PartitionKind::Fs { fs_type } => {
                    let fs_uuid = blkid_uuid(&dev);
                    let fs_mkfs_params = probe_fs_params(&dev, &fs_type);
                    PartitionHandle::Fs(FsHandle::new_with_uuid(
                        desc,
                        fs_uuid,
                        fs_mkfs_params,
                        move || mount_partition_ro(&dev, &fs_type, Arc::clone(&nbd)),
                    ))
                }
                PartitionKind::MbrBootCode => {
                    // MbrBootCode should never appear in part_infos (only in
                    // the synthetic prepended handle above).  Skip defensively.
                    continue;
                }
            };
            handles.push(handle);
        }

        Ok(handles)
    }

    /// Create a writable partition handle for an output qcow2 image.
    ///
    /// This method is called on an [`OpenQcow2Image`] that was returned by
    /// [`Qcow2Image::create`] (RW-connected NBD).  It:
    /// 1. Waits for the partition device node to be stable.
    /// 2. For `Fs` partitions: runs `mkfs`, returns an [`FsHandle`] whose
    ///    `mount_fn` mounts the freshly-formatted partition RW.
    /// 3. For binary partitions: returns a writable handle whose `write_fn`
    ///    writes the raw bytes directly to the block device node.
    /// 4. For `MbrBootCode`: returns an [`MbrHandle`] whose `write_fn` calls
    ///    `write_raw_disk_bytes` at offset 0 on the raw NBD device.
    fn create_partition(&self, pm: &PartitionManifest) -> crate::Result<PartitionHandle> {
        let nbd_device = &self.nbd.0;
        let desc = pm.descriptor.clone();

        match &pm.content {
            PartitionContent::MbrBootCode { .. } => {
                // MBR boot-code goes to offset 0 of the raw disk — not a partition node.
                let disk = nbd_device.to_string();
                let handle = MbrHandle::new_rw(
                    desc,
                    || {
                        Err(crate::Error::Format(
                            "output Mbr handle: read not supported".into(),
                        ))
                    },
                    move |data| write_raw_disk_bytes(&disk, 0, data),
                );
                Ok(PartitionHandle::Mbr(handle))
            }

            PartitionContent::BiosBoot { .. } => {
                let part_num = desc.number;
                let stable_dev =
                    wait_for_partition_stable(nbd_device, part_num, NBD_PARTITION_TIMEOUT)?;
                let dev = stable_dev.clone();
                let handle = BiosBootHandle::new_rw(
                    desc,
                    || {
                        Err(crate::Error::Format(
                            "output BiosBoot handle: read not supported".into(),
                        ))
                    },
                    move |data| {
                        let mut f = std::fs::OpenOptions::new()
                            .write(true)
                            .open(&dev)
                            .map_err(|e| crate::Error::Format(format!("open {dev}: {e}")))?;
                        f.write_all(data)
                            .map_err(|e| crate::Error::Format(format!("write {dev}: {e}")))
                    },
                );
                Ok(PartitionHandle::BiosBoot(handle))
            }

            PartitionContent::Raw { .. } => {
                let part_num = desc.number;
                let stable_dev =
                    wait_for_partition_stable(nbd_device, part_num, NBD_PARTITION_TIMEOUT)?;
                let dev = stable_dev.clone();
                let handle = RawHandle::new_rw(
                    desc,
                    || {
                        Err(crate::Error::Format(
                            "output Raw handle: read not supported".into(),
                        ))
                    },
                    move |data| {
                        let mut f = std::fs::OpenOptions::new()
                            .write(true)
                            .open(&dev)
                            .map_err(|e| crate::Error::Format(format!("open {dev}: {e}")))?;
                        f.write_all(data)
                            .map_err(|e| crate::Error::Format(format!("write {dev}: {e}")))
                    },
                );
                Ok(PartitionHandle::Raw(handle))
            }

            PartitionContent::Fs {
                fs_type,
                fs_uuid,
                fs_mkfs_params,
                ..
            } => {
                let part_num = desc.number;
                let stable_dev =
                    wait_for_partition_stable(nbd_device, part_num, NBD_PARTITION_TIMEOUT)?;
                // Format the partition before returning the handle — mkfs must
                // happen before any attempt to mount.
                mkfs_partition(
                    &stable_dev,
                    fs_type,
                    fs_uuid.as_deref(),
                    fs_mkfs_params.as_ref(),
                )?;
                let dev = stable_dev;
                let fs = fs_type.clone();
                let fh = FsHandle::new(desc, move || {
                    let dir = TempDir::new()
                        .map_err(|e| crate::Error::Format(format!("TempDir::new: {e}")))?;
                    mount_partition_rw_plain(&dev, &fs, dir.path())?;
                    let root = dir.path().to_path_buf();
                    Ok(Box::new(RwOutputMountHandle { _dir: dir, root }) as Box<dyn MountHandle>)
                });
                Ok(PartitionHandle::Fs(fh))
            }
        }
    }
}

// ── open() helpers ────────────────────────────────────────────────────────────

/// Read exactly `size_bytes` from a block device file descriptor.
fn read_block_device_bytes(dev: &str, size_bytes: usize) -> crate::Result<Vec<u8>> {
    use std::io::Read;
    let mut f =
        fs::File::open(dev).map_err(|e| crate::Error::Format(format!("open({dev}): {e}")))?;
    let mut buf = vec![0u8; size_bytes];
    f.read_exact(&mut buf)
        .map_err(|e| crate::Error::Format(format!("read_exact({dev}, {size_bytes}B): {e}")))?;
    Ok(buf)
}

/// Read `len` bytes from the raw block device at the given byte `offset`.
///
/// Used to capture the MBR boot-code area (bytes 0–439) before any partition
/// table manipulation overwrites it.
fn read_raw_disk_bytes(dev: &str, offset: u64, len: usize) -> crate::Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f =
        fs::File::open(dev).map_err(|e| crate::Error::Format(format!("open({dev}): {e}")))?;
    f.seek(SeekFrom::Start(offset))
        .map_err(|e| crate::Error::Format(format!("seek({dev}, {offset}): {e}")))?;
    let mut buf = vec![0u8; len];
    f.read_exact(&mut buf)
        .map_err(|e| crate::Error::Format(format!("read_exact({dev}, {len}B @ {offset}): {e}")))?;
    Ok(buf)
}

/// Write `data` to the raw block device at the given byte `offset`.
///
/// Used to restore the MBR boot-code area after `sgdisk --clear` rewrites it.
fn write_raw_disk_bytes(dev: &str, offset: u64, data: &[u8]) -> crate::Result<()> {
    use std::io::{Seek, SeekFrom, Write};
    let mut f = fs::OpenOptions::new()
        .write(true)
        .open(dev)
        .map_err(|e| crate::Error::Format(format!("open({dev}): {e}")))?;
    f.seek(SeekFrom::Start(offset))
        .map_err(|e| crate::Error::Format(format!("seek({dev}, {offset}): {e}")))?;
    f.write_all(data).map_err(|e| {
        crate::Error::Format(format!("write_all({dev}, {}B @ {offset}): {e}", data.len()))
    })?;
    Ok(())
}

/// Mount `device` read-only with the given `fs_type` and return a RAII handle.
///
/// For XFS, `norecovery` is added to `mount(2)` data to avoid journal replay
/// errors on a potentially dirty (but read-only) device.
fn mount_partition_ro(
    device: &str,
    fs_type: &str,
    nbd: Arc<NbdConn>,
) -> crate::Result<Box<dyn MountHandle>> {
    use nix::mount::{mount, MsFlags};

    let mount_dir =
        TempDir::new().map_err(|e| crate::Error::Format(format!("TempDir::new: {e}")))?;
    let root = mount_dir.path().to_path_buf();

    debug!(device, fs_type, mount_root = %root.display(), "mount_partition_ro: mounting");
    let flags = MsFlags::MS_RDONLY;
    // ext4 and XFS need `norecovery` when mounted read-only with a dirty
    // journal; without it the kernel tries to replay the journal but cannot
    // write, returning EROFS ("Read-only file system").
    // XFS also needs `nouuid` to allow mounting multiple XFS filesystems with
    // the same UUID simultaneously (base and target partitions share UUID).
    let extra: Option<&str> = if fs_type == "xfs" {
        Some("norecovery,nouuid")
    } else if fs_type == "ext4" {
        Some("norecovery")
    } else {
        None
    };

    mount(Some(device), root.as_path(), Some(fs_type), flags, extra)
        .map_err(|e| crate::Error::Format(format!("mount({device}, {fs_type}): {e}")))?;

    Ok(Box::new(PartitionMountHandle {
        _mount_dir: mount_dir,
        root,
        _nbd: nbd,
    }))
}

/// Run `blkid <device>` and extract the `TYPE=` value.
///
/// Returns `None` when blkid reports no recognized filesystem type (e.g.
/// BIOS-boot, Linux swap, or an empty partition).
fn blkid_fs_type(device: &str) -> Option<String> {
    let out = Command::new("blkid").arg(device).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    // blkid output: /dev/nbdNpM: UUID="..." TYPE="ext4" PARTUUID="..."
    for token in s.split_ascii_whitespace() {
        if let Some(rest) = token.strip_prefix("TYPE=\"") {
            return Some(rest.trim_end_matches('"').to_string());
        }
    }
    None
}

/// Run `blkid <device>` and extract the `UUID=` value.
///
/// Returns `None` when blkid reports no UUID (e.g. BIOS-boot partition).
fn blkid_uuid(device: &str) -> Option<String> {
    let out = Command::new("blkid").arg(device).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    for token in s.split_ascii_whitespace() {
        if let Some(rest) = token.strip_prefix("UUID=\"") {
            return Some(rest.trim_end_matches('"').to_string());
        }
    }
    None
}

/// Probe filesystem geometry and feature flags from an existing block device.
///
/// Returns a `HashMap` suitable for storing in [`PartitionContent::Fs`] as
/// `fs_mkfs_params` so that decompression can call `mkfs` with the exact same
/// parameters.  Returns `None` if the filesystem type is unsupported or the
/// probe tool is unavailable.
///
/// Supported:
/// - `"xfs"` — reads `xfs_info <device>` (works on unmounted block devices)
/// - `"ext4"` — reads `tune2fs -l <device>`
/// - `"vfat"` / `"fat32"` / `"fat16"` — reads `fsstat <device>` when available
fn probe_fs_params(
    device: &str,
    fs_type: &str,
) -> Option<std::collections::HashMap<String, String>> {
    match fs_type {
        "xfs" => probe_xfs_params(device),
        "ext4" => probe_ext4_params(device),
        "vfat" | "fat32" | "fat16" => probe_fat_params(device),
        _ => None,
    }
}

/// Parse `xfs_info <device>` output into mkfs-reproducible parameters.
///
/// Example xfs_info output:
/// ```text
/// meta-data=/dev/nbd0p2  isize=512    agcount=4, agsize=655360 blks
///          =             sectsz=512   attr=2, projid32bit=1
///          =             crc=1        finobt=1, sparse=1, rmapbt=0
///          =             reflink=1    bigtime=1 inobtcount=1 nrext64=0
/// data     =             bsize=4096   blocks=2621440, imaxpct=25
///          =             sunit=0      swidth=0 blks
/// naming   =version 2   bsize=4096   ascii-ci=0, ftype=1
/// log      =internal log bsize=4096  blocks=16384, version=2
///          =             sectsz=512   sunit=0 blks, lazy-count=1
/// realtime =none         extsz=4096   blocks=0, rtextents=0
/// ```
fn probe_xfs_params(device: &str) -> Option<std::collections::HashMap<String, String>> {
    let out = Command::new("xfs_info").arg(device).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut p: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    for line in text.lines() {
        // Split on whitespace; each key=value token (or key=value, with trailing
        // comma) is parsed independently.
        for token in line.split_ascii_whitespace() {
            let token = token.trim_end_matches(',');
            if let Some((k, v)) = token.split_once('=') {
                match k {
                    "isize" => {
                        p.insert("inode_size".into(), v.into());
                    }
                    "bsize" => {
                        // bsize appears in multiple sections; only take the
                        // one from the "data" section (first occurrence).
                        p.entry("block_size".into()).or_insert_with(|| v.into());
                    }
                    "sectsz" => {
                        p.entry("sector_size".into()).or_insert_with(|| v.into());
                    }
                    "crc" => {
                        p.insert("crc".into(), v.into());
                    }
                    "finobt" => {
                        p.insert("finobt".into(), v.into());
                    }
                    "sparse" => {
                        p.insert("sparse".into(), v.into());
                    }
                    "rmapbt" => {
                        p.insert("rmapbt".into(), v.into());
                    }
                    "reflink" => {
                        p.insert("reflink".into(), v.into());
                    }
                    "bigtime" => {
                        p.insert("bigtime".into(), v.into());
                    }
                    "inobtcount" => {
                        p.insert("inobtcount".into(), v.into());
                    }
                    "ftype" => {
                        p.insert("ftype".into(), v.into());
                    }
                    _ => {}
                }
            }
        }
    }

    if p.is_empty() {
        None
    } else {
        Some(p)
    }
}

/// Parse `tune2fs -l <device>` output into mkfs-reproducible parameters.
fn probe_ext4_params(device: &str) -> Option<std::collections::HashMap<String, String>> {
    let out = Command::new("tune2fs").args(["-l", device]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut p: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("Block size:") {
            p.insert("block_size".into(), rest.trim().into());
        } else if let Some(rest) = line.strip_prefix("Inode size:") {
            p.insert("inode_size".into(), rest.trim().into());
        } else if let Some(rest) = line.strip_prefix("Filesystem features:") {
            p.insert("features".into(), rest.trim().into());
        } else if let Some(rest) = line.strip_prefix("Filesystem volume name:") {
            let label = rest.trim();
            if label != "<none>" {
                p.insert("label".into(), label.into());
            }
        }
    }

    if p.is_empty() {
        None
    } else {
        Some(p)
    }
}

/// Probe FAT filesystem metadata (sectors-per-cluster, label).
///
/// Tries `fsstat` (from The Sleuth Kit); silently returns `None` if
/// unavailable so that decompression falls back to safe defaults.
fn probe_fat_params(device: &str) -> Option<std::collections::HashMap<String, String>> {
    let out = Command::new("fsstat").arg(device).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut p: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("Sectors Per Cluster:") {
            p.insert("sectors_per_cluster".into(), rest.trim().into());
        } else if let Some(rest) = line.strip_prefix("Volume Label (Boot Sector):") {
            let label = rest.trim();
            if !label.is_empty() {
                p.insert("label".into(), label.into());
            }
        }
    }

    if p.is_empty() {
        None
    } else {
        Some(p)
    }
}

/// Extract the partition number from a node path like `/dev/nbd3p2` → 2.
///
/// Finds the last `p` in the path and parses the trailing digits.
fn node_partition_number(node: &str) -> u32 {
    if let Some(pos) = node.rfind('p') {
        if let Ok(n) = node[pos + 1..].parse::<u32>() {
            return n;
        }
    }
    1
}

/// Determine the [`PartitionKind`] from a GPT type GUID and `blkid` output.
///
/// Comparison is case-insensitive.  For unknown or Linux-data GUIDs,
/// `blkid` is used to probe the filesystem type.
fn classify_partition(type_guid: Option<&str>, block_dev: &str) -> PartitionKind {
    let guid_lc = type_guid
        .map(|g| g.to_ascii_lowercase())
        .unwrap_or_default();

    match guid_lc.as_str() {
        g if g == GUID_BIOS_BOOT => PartitionKind::BiosBoot,
        g if g == GUID_LINUX_SWAP => PartitionKind::Raw,
        g if g == GUID_EFI_SYSTEM => PartitionKind::Fs {
            fs_type: "vfat".into(),
        },
        _ => match blkid_fs_type(block_dev) {
            Some(fs) => PartitionKind::Fs { fs_type: fs },
            None => PartitionKind::Raw,
        },
    }
}

/// Run `sfdisk --json <nbd_device>` and parse the output into a [`DiskLayout`].
///
/// Each partition's [`PartitionKind`] is determined by combining the GPT type
/// GUID (from sfdisk) with a `blkid` probe on the partition device node.
fn parse_disk_layout(nbd_device: &str) -> crate::Result<DiskLayout> {
    let out = Command::new("sfdisk")
        .args(["--json", nbd_device])
        .output()
        .map_err(|e| crate::Error::Format(format!("failed to spawn sfdisk: {e}")))?;

    if !out.status.success() || out.stdout.is_empty() {
        return Err(crate::Error::Format(format!(
            "sfdisk --json {nbd_device} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }

    let parsed: SfdiskOutput = serde_json::from_slice(&out.stdout)
        .map_err(|e| crate::Error::Format(format!("parse sfdisk JSON: {e}")))?;

    let table = parsed.partitiontable;

    let scheme = match table.label.as_str() {
        "gpt" => DiskScheme::Gpt,
        "dos" => DiskScheme::Mbr,
        other => {
            return Err(crate::Error::Format(format!(
                "unsupported partition table label: {other}"
            )));
        }
    };

    let disk_guid = if scheme == DiskScheme::Gpt {
        table
            .id
            .as_deref()
            .and_then(|s| s.parse::<uuid::Uuid>().ok())
    } else {
        None
    };

    let mut partitions = Vec::new();
    for p in &table.partitions {
        let part_num = node_partition_number(&p.node);
        let kind = classify_partition(p.part_type.as_deref(), &p.node);
        let size_bytes = p.size * table.sectorsize;

        let partition_guid = if scheme == DiskScheme::Gpt {
            p.uuid.as_deref().and_then(|s| s.parse::<uuid::Uuid>().ok())
        } else {
            None
        };
        let type_guid = if scheme == DiskScheme::Gpt {
            p.part_type
                .as_deref()
                .and_then(|s| s.parse::<uuid::Uuid>().ok())
        } else {
            None
        };

        partitions.push(PartitionDescriptor {
            number: part_num,
            partition_guid,
            type_guid,
            name: if p.name.as_deref().unwrap_or("").is_empty() {
                None
            } else {
                p.name.clone()
            },
            start_lba: p.start,
            end_lba: p.start + p.size - 1,
            size_bytes,
            flags: 0,
            kind,
        });
    }

    partitions.sort_by_key(|p| p.number);

    Ok(DiskLayout {
        scheme,
        disk_guid,
        partitions,
    })
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
/// # Pack behaviour
///
/// - **No base image** (`Qcow2Image::new()`): creates a fresh qcow2 with a
///   single raw ext4 partition from `source_dir`.  Use for testing.
/// - **With base image** (`Qcow2Image::with_base(base_path)`): copies the
///   base qcow2, identifies the main mountable partition via `blkid`, wipes
///   and reformats just that partition, then copies `source_dir` into it.
///   All other partitions (e.g. BIOS boot, EFI) remain intact.
pub struct Qcow2Image {
    /// When set, `pack()` clones this image and replaces its main partition.
    base_image: Option<PathBuf>,
}

impl Qcow2Image {
    /// Create a `Qcow2Image` that builds images from scratch (no base).
    pub fn new() -> Self {
        Self { base_image: None }
    }

    /// Create a `Qcow2Image` that clones `base` and replaces its main
    /// filesystem partition during `pack()`.
    pub fn with_base(base: PathBuf) -> Self {
        Self {
            base_image: Some(base),
        }
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

    /// Open a `.qcow2` file, parse its partition table via `sfdisk`, and
    /// return an [`OpenImage`] that holds the NBD connection for the lifetime
    /// of the returned handle.
    ///
    /// # Steps
    ///
    /// 1. Hold [`NBD_ALLOC`], find a free `/dev/nbdN`, connect read-only.
    /// 2. Wait for `/dev/nbdNp1` to appear (kernel partition re-read).
    /// 3. `sfdisk --json /dev/nbdN` → parse into [`DiskLayout`].
    /// 4. Classify each partition via GPT type GUID + `blkid`.
    /// 5. Return [`OpenQcow2Image`].
    ///
    /// The NBD connection is released (via [`NbdConn`] RAII) when both the
    /// [`OpenQcow2Image`] **and** all [`PartitionHandle`]s derived from it are
    /// dropped.
    ///
    /// # Environment variables
    ///
    /// - `QCOW2_DEVICE=N` — start scanning for free NBD at index N (default: 0).
    fn open(&self, path: &Path) -> crate::Result<Box<dyn OpenImage>> {
        let start_index: u32 = std::env::var("QCOW2_DEVICE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);

        // 1. Allocate a free NBD device and connect read-only.
        let nbd_device = {
            let _guard = NBD_ALLOC
                .lock()
                .map_err(|_| crate::Error::Format("NBD_ALLOC mutex poisoned".into()))?;
            let dev = find_free_nbd(start_index)?;
            nbd_connect(&dev, path)?;
            // Wait until the device size becomes non-zero (kernel has completed
            // the NBD handshake). Must hold NBD_ALLOC so concurrent open()
            // calls see this device as busy.
            wait_for_nbd_connected(&dev);
            dev
        };

        // Flush the block-device page cache to ensure that any pages cached by
        // a prior connection (possibly to a different qcow2 image) are evicted.
        // This prevents stale reads after NBD device reuse.
        let _ = Command::new("blockdev")
            .args(["--flushbufs", &nbd_device])
            .output();

        debug!(qcow2 = %path.display(), nbd = %nbd_device, "open: connected qcow2 to nbd device");

        // Wrap in Arc so partition handles share the NBD connection lifetime.
        let nbd = Arc::new(NbdConn(nbd_device.clone()));

        // 2. Wait for partition device nodes (kernel re-reads partition table).
        let _ = wait_for_block_device(&nbd_device);

        // 3. Parse the partition table.  On failure the NbdConn Arc will
        // disconnect automatically when it drops at end of scope.
        let layout = parse_disk_layout(&nbd_device)?;

        // 4. Read the MBR boot-code area (first 440 bytes of the raw device).
        //    Captured as the synthetic MBR partition (number 0) so the
        //    compressor stores it as PartitionContent::MbrBootCode and the
        //    decompressor can restore it after sgdisk --clear.  Non-fatal.
        let mbr_bytes = read_raw_disk_bytes(&nbd_device, 0, 440).ok().map(Arc::new);

        // 5. Build PartInfo for each partition.
        let part_infos: Vec<PartInfo> = layout
            .partitions
            .iter()
            .map(|desc| PartInfo {
                desc: desc.clone(),
                block_dev: format!("{nbd_device}p{}", desc.number),
            })
            .collect();

        Ok(Box::new(OpenQcow2Image {
            layout,
            part_infos,
            nbd,
            mbr_bytes,
        }))
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
            wait_for_nbd_connected(&dev);
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

    /// Pack the filesystem tree at `source_dir` into a qcow2 image at `output_path`.
    ///
    /// **No-base mode** (`Qcow2Image::new()`):
    /// Creates a fresh single-partition ext4 qcow2.  Suitable for tests.
    ///
    /// **Base-image mode** (`Qcow2Image::with_base(base)`):
    /// 1. `cp --sparse=always base output` — clone the full qcow2 including all partitions
    /// 2. `qemu-nbd --connect` (writable) on the clone
    /// 3. `find_main_partition` — `blkid` or `QCOW2_PARTITION` to locate the FS partition
    /// 4. `mkfs.ext4 -F <partition>` — wipe and reformat just the main partition
    /// 5. `mount(2)` read-write + `cp -a source_dir/. mountpoint/`
    /// 6. `umount(2)` + `qemu-nbd --disconnect`
    ///
    /// All other partitions (e.g. p1 BIOS boot) remain identical to the base.
    fn pack(&self, source_dir: &Path, output_path: &Path) -> Result<()> {
        if let Some(base) = &self.base_image {
            pack_with_base(base, source_dir, output_path)
        } else {
            pack_fresh(source_dir, output_path)
        }
    }

    /// Create a new, empty qcow2 image at `path` with the GPT layout from
    /// `layout`, connect it RW via NBD, and return a writable [`OpenQcow2Image`]
    /// whose [`create_partition`][crate::formats::OpenImage::create_partition]
    /// prepares each partition (mkfs for Fs, direct write for binary types).
    ///
    /// Steps:
    /// 1. `qemu-img create -f qcow2 path <size>`
    /// 2. `qemu-nbd --connect` (writable)
    /// 3. `sgdisk` to write the GPT
    /// 4. Wait for all partition nodes to be stable
    /// 5. Return [`OpenQcow2Image`] with RW NBD alive
    fn create(&self, path: &Path, layout: &DiskLayout) -> Result<Box<dyn OpenImage>> {
        let image_size = calculate_image_size(layout);
        run_command(
            Command::new("qemu-img").args([
                "create",
                "-f",
                "qcow2",
                path.to_str()
                    .ok_or_else(|| crate::Error::Format("non-UTF-8 output path".into()))?,
                &image_size.to_string(),
            ]),
            "qemu-img create",
        )?;

        let start_index: u32 = std::env::var("QCOW2_DEVICE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let nbd_device = {
            let _guard = NBD_ALLOC
                .lock()
                .map_err(|_| crate::Error::Format("NBD_ALLOC mutex poisoned".into()))?;
            let dev = find_free_nbd(start_index)?;
            nbd_connect_rw(&dev, path)?;
            wait_for_nbd_connected(&dev);
            dev
        };
        let nbd = Arc::new(NbdConn(nbd_device.clone()));

        write_gpt(&nbd_device, layout)?;

        // Wait for partition nodes: first sentinel (p1), then each partition.
        let _ = wait_for_block_device(&nbd_device);
        for p in &layout.partitions {
            let _ = wait_for_partition_stable(&nbd_device, p.number, NBD_PARTITION_TIMEOUT);
        }

        // Build PartInfo from layout — no need to re-parse with sfdisk since
        // we just wrote the GPT ourselves.
        let part_infos: Vec<PartInfo> = layout
            .partitions
            .iter()
            .map(|p| PartInfo {
                desc: p.clone(),
                block_dev: format!("{nbd_device}p{}", p.number),
            })
            .collect();

        Ok(Box::new(OpenQcow2Image {
            layout: layout.clone(),
            part_infos,
            nbd,
            // New blank image has no pre-existing MBR boot code; the
            // MbrBootCode partition (if any) is written via create_partition.
            mbr_bytes: None,
        }))
    }
}

// ── pack_from_manifest helpers ────────────────────────────────────────────────

/// Calculate the total qcow2 image size in bytes from a [`DiskLayout`].
///
/// Takes the highest `end_lba` across all partitions, adds the 33-sector GPT
/// backup area (32 partition-entry sectors + 1 backup header), and rounds up
/// to a 1 MiB boundary.
fn calculate_image_size(layout: &DiskLayout) -> u64 {
    let max_end_lba = layout
        .partitions
        .iter()
        .map(|p| p.end_lba)
        .max()
        .unwrap_or(2047); // minimum sane GPT disk

    // GPT backup area: 33 LBA (32 partition-entry blocks + backup header).
    let total_sectors = max_end_lba + 34;
    let size_bytes = total_sectors * SECTOR_SIZE;

    // Round up to 1 MiB boundary.
    (size_bytes + (1024 * 1024 - 1)) & !(1024 * 1024 - 1)
}

/// Write a GPT partition table to `nbd_device` using `sgdisk`.
///
/// Only [`DiskScheme::Gpt`] is supported; other schemes return an error.
fn write_gpt(nbd_device: &str, layout: &DiskLayout) -> crate::Result<()> {
    if layout.scheme != DiskScheme::Gpt {
        return Err(crate::Error::Format(format!(
            "pack_from_manifest: only GPT is supported (got {:?})",
            layout.scheme
        )));
    }

    let mut args: Vec<String> = vec!["--clear".into()];

    if let Some(guid) = layout.disk_guid {
        args.push(format!("--disk-guid={guid}"));
    }

    for p in &layout.partitions {
        args.push(format!("--new={}:{}:{}", p.number, p.start_lba, p.end_lba));
        if let Some(tg) = p.type_guid {
            args.push(format!("--typecode={}:{}", p.number, tg));
        }
        if let Some(pg) = p.partition_guid {
            args.push(format!("--partition-guid={}:{}", p.number, pg));
        }
        if let Some(name) = &p.name {
            if !name.is_empty() {
                args.push(format!("--change-name={}:{}", p.number, name));
            }
        }
    }

    args.push(nbd_device.into());
    run_command(Command::new("sgdisk").args(&args), "sgdisk")?;

    // Force the kernel to re-read the partition table.  sgdisk sends
    // BLKRRPART, but NBD devices sometimes need an explicit nudge.
    let _ = Command::new("partprobe").arg(nbd_device).output();
    let _ = Command::new("blockdev")
        .args(["--rereadpt", nbd_device])
        .output();

    Ok(())
}

/// Format a partition with the appropriate `mkfs` tool.
fn mkfs_partition(
    part_dev: &str,
    fs_type: &str,
    fs_uuid: Option<&str>,
    fs_mkfs_params: Option<&std::collections::HashMap<String, String>>,
) -> crate::Result<()> {
    match fs_type {
        "ext4" => {
            let mut cmd = Command::new("mkfs.ext4");
            // Start from the features stored at compression time.  If none are
            // recorded fall back to the safe GRUB-compatible defaults.
            if let Some(features) = fs_mkfs_params.and_then(|p| p.get("features")) {
                // tune2fs -l lists features separated by spaces; mkfs.ext4 -O
                // accepts a comma-separated list.
                let comma_features = features
                    .split_ascii_whitespace()
                    .collect::<Vec<_>>()
                    .join(",");
                cmd.args(["-F", "-O", &comma_features]);
            } else {
                // Disable features not supported by GRUB 2.06's embedded ext2
                // driver when no stored params are available:
                //   - orphan_file: added in e2fsprogs 1.46.2+
                //   - metadata_csum_seed: added in e2fsprogs 1.46.4+
                cmd.args(["-F", "-O", "^orphan_file,^metadata_csum_seed"]);
            }
            if let Some(params) = fs_mkfs_params {
                if let Some(bs) = params.get("block_size") {
                    cmd.args(["-b", bs]);
                }
                if let Some(is) = params.get("inode_size") {
                    cmd.args(["-I", is]);
                }
                if let Some(label) = params.get("label") {
                    cmd.args(["-L", label]);
                }
            }
            if let Some(uuid) = fs_uuid {
                cmd.args(["-U", uuid]);
            }
            cmd.arg(part_dev);
            run_command(&mut cmd, "mkfs.ext4")
        }
        "xfs" => {
            let mut cmd = Command::new("mkfs.xfs");
            cmd.arg("-f");
            // Build the -m metadata suboption string from stored params.
            let mut m_opts: Vec<String> = Vec::new();
            if let Some(uuid) = fs_uuid {
                // Pass UUID via metadata section so GRUB's `search --fs-uuid`
                // finds the correct partition after decompression.
                m_opts.push(format!("uuid={uuid}"));
            }
            if let Some(params) = fs_mkfs_params {
                for key in &[
                    "crc",
                    "finobt",
                    "rmapbt",
                    "reflink",
                    "bigtime",
                    "inobtcount",
                ] {
                    if let Some(val) = params.get(*key) {
                        m_opts.push(format!("{key}={val}"));
                    }
                }
                let mut i_opts: Vec<String> = Vec::new();
                if let Some(bs) = params.get("block_size") {
                    cmd.args(["-b", &format!("size={bs}")]);
                }
                if let Some(is) = params.get("inode_size") {
                    i_opts.push(format!("size={is}"));
                }
                if let Some(sparse) = params.get("sparse") {
                    // sparse is an inode suboption in mkfs.xfs >= 5.x
                    i_opts.push(format!("sparse={sparse}"));
                }
                if !i_opts.is_empty() {
                    cmd.args(["-i", &i_opts.join(",")]);
                }
                if let Some(ss) = params.get("sector_size") {
                    cmd.args(["-s", &format!("size={ss}")]);
                }
                if let Some(ftype) = params.get("ftype") {
                    cmd.args(["-n", &format!("ftype={ftype}")]);
                }
                if let Some(label) = params.get("label") {
                    cmd.args(["-L", label]);
                }
            }
            if !m_opts.is_empty() {
                cmd.args(["-m", &m_opts.join(",")]);
            }
            cmd.arg(part_dev);
            run_command(&mut cmd, "mkfs.xfs")
        }
        "vfat" | "fat32" | "fat16" => {
            let mut cmd = Command::new("mkfs.fat");
            cmd.args(["-F", "32"]);
            if let Some(uuid) = fs_uuid {
                // mkfs.fat accepts the volume-id as a 32-bit hex value;
                // take the first 8 hex digits of the filesystem UUID.
                let vol_id: String = uuid
                    .chars()
                    .filter(|c| c.is_ascii_hexdigit())
                    .take(8)
                    .collect();
                if !vol_id.is_empty() {
                    cmd.args(["-i", &vol_id]);
                }
            }
            if let Some(params) = fs_mkfs_params {
                if let Some(spc) = params.get("sectors_per_cluster") {
                    cmd.args(["-s", spc]);
                }
                if let Some(label) = params.get("label") {
                    cmd.args(["-n", label]);
                }
            }
            cmd.arg(part_dev);
            run_command(&mut cmd, "mkfs.fat")
        }
        other => Err(crate::Error::Format(format!(
            "mkfs_partition: unsupported fs_type '{other}'"
        ))),
    }
}

/// Mount `device` read-write at `mount_point` with `fs_type`.
fn mount_partition_rw_plain(device: &str, fs_type: &str, mount_point: &Path) -> crate::Result<()> {
    use nix::mount::{mount, MsFlags};
    mount(
        Some(device),
        mount_point,
        Some(fs_type),
        MsFlags::empty(),
        None::<&str>,
    )
    .map_err(|e| crate::Error::Format(format!("mount_rw({device}, {fs_type}): {e}")))
}
