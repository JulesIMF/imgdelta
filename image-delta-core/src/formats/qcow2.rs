#![cfg(feature = "qcow2")]

use crate::{Image, MountHandle, Result};
use std::path::{Path, PathBuf};

/// RAII handle for a qcow2 image mounted via `qemu-nbd`.
///
/// Dropping this handle will:
/// 1. `umount` the mount point
/// 2. `qemu-nbd --disconnect` the NBD device
/// 3. Remove the temporary mount directory
///
/// Phase 5 implementation.
pub struct Qcow2MountHandle {
    root: PathBuf,
    // Phase 5: nbd_device: String, _temp_dir: tempfile::TempDir
}

impl MountHandle for Qcow2MountHandle {
    fn root(&self) -> &Path {
        &self.root
    }
}

/// [`Image`] implementation for qcow2 VM disk images.
///
/// Requires:
/// - Linux
/// - `qemu-nbd` in PATH
/// - `CAP_SYS_ADMIN` or `sudo` access for mount/umount
///
/// Feature-gated behind `feature = "qcow2"`.
pub struct Qcow2Image;

impl Qcow2Image {
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

    fn mount(&self, _path: &Path) -> Result<Box<dyn MountHandle>> {
        todo!("Phase 5: capabilities check → qemu-nbd attach → mount → return handle")
    }

    fn pack(&self, source_dir: &Path, output_path: &Path) -> Result<()> {
        todo!("Phase 5: create qcow2 image from source_dir contents (e.g. via qemu-img)")
    }
}
