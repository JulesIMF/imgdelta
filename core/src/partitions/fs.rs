// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// FsHandle — handle for filesystem partitions

use std::path::Path;

use super::PartitionDescriptor;

// ── MountHandle ───────────────────────────────────────────────────────────────

/// RAII handle to a mounted or otherwise accessible filesystem root.
///
/// Dropping the handle must unmount/clean up any associated resources
/// (e.g. detach qemu-nbd device, remove temp directory).
pub trait MountHandle: Send {
    /// Path to the directory that is the root of the mounted filesystem.
    fn root(&self) -> &Path;
}

/// Simple [`MountHandle`] that wraps a [`PathBuf`].
///
/// Used by [`DirectoryImage`] and in tests — no OS-level mount needed.
///
/// [`DirectoryImage`]: crate::DirectoryImage
pub struct SimpleMountHandle {
    root: std::path::PathBuf,
}

impl SimpleMountHandle {
    pub fn new(root: std::path::PathBuf) -> Self {
        Self { root }
    }
}

impl MountHandle for SimpleMountHandle {
    fn root(&self) -> &Path {
        &self.root
    }
}

// ── FsHandle ──────────────────────────────────────────────────────────────────

/// Handle to a filesystem partition.
///
/// Calling [`mount()`][FsHandle::mount] returns a [`MountHandle`] that exposes
/// the partition root as a directory path.  For [`DirectoryImage`] this is
/// always a no-op.  For `Qcow2Image` this mounts the block device.
///
/// [`DirectoryImage`]: crate::DirectoryImage
pub struct FsHandle {
    /// Partition descriptor from the image's partition table.
    pub descriptor: PartitionDescriptor,
    /// Filesystem UUID (e.g. ext4 superblock UUID), if known at open time.
    pub fs_uuid: Option<String>,
    mount_fn: Box<dyn Fn() -> crate::Result<Box<dyn MountHandle>> + Send + Sync>,
}

impl FsHandle {
    /// Create a new [`FsHandle`] with the given descriptor and mount closure.
    pub fn new(
        descriptor: PartitionDescriptor,
        mount_fn: impl Fn() -> crate::Result<Box<dyn MountHandle>> + Send + Sync + 'static,
    ) -> Self {
        let uuid_str = descriptor.type_guid.map(|uuid| uuid.to_string());
        Self {
            descriptor,
            fs_uuid: uuid_str,
            mount_fn: Box::new(mount_fn),
        }
    }

    /// Create a new [`FsHandle`] with a known filesystem UUID.
    pub fn new_with_uuid(
        descriptor: PartitionDescriptor,
        fs_uuid: Option<String>,
        mount_fn: impl Fn() -> crate::Result<Box<dyn MountHandle>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            descriptor,
            fs_uuid,
            mount_fn: Box::new(mount_fn),
        }
    }

    /// Mount the partition and return an RAII handle to its root directory.
    pub fn mount(&self) -> crate::Result<Box<dyn MountHandle>> {
        (self.mount_fn)()
    }
}
