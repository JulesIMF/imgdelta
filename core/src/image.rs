// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Image and OpenImage traits; PartitionHandle, FsHandle, BiosBootHandle, RawHandle

use std::path::{Path, PathBuf};

use crate::partition::{DiskLayout, DiskScheme, PartitionDescriptor};

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
    root: PathBuf,
}

impl SimpleMountHandle {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl MountHandle for SimpleMountHandle {
    fn root(&self) -> &Path {
        &self.root
    }
}

// ── Partition handles ─────────────────────────────────────────────────────────

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
    mount_fn: Box<dyn Fn() -> crate::Result<Box<dyn MountHandle>> + Send + Sync>,
}

impl FsHandle {
    /// Create a new [`FsHandle`] with the given descriptor and mount closure.
    pub fn new(
        descriptor: PartitionDescriptor,
        mount_fn: impl Fn() -> crate::Result<Box<dyn MountHandle>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            descriptor,
            mount_fn: Box::new(mount_fn),
        }
    }

    /// Mount the partition and return an RAII handle to its root directory.
    pub fn mount(&self) -> crate::Result<Box<dyn MountHandle>> {
        (self.mount_fn)()
    }
}

/// Handle to a BIOS-boot (raw bytes) partition.
pub struct BiosBootHandle {
    /// Partition descriptor from the image's partition table.
    pub descriptor: PartitionDescriptor,
    read_fn: Box<dyn Fn() -> crate::Result<Vec<u8>> + Send + Sync>,
}

impl BiosBootHandle {
    /// Create a new [`BiosBootHandle`].
    pub fn new(
        descriptor: PartitionDescriptor,
        read_fn: impl Fn() -> crate::Result<Vec<u8>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            descriptor,
            read_fn: Box::new(read_fn),
        }
    }

    /// Read the raw bytes of this partition.
    pub fn read_raw(&self) -> crate::Result<Vec<u8>> {
        (self.read_fn)()
    }
}

/// Handle to a raw (unformatted) partition.
pub struct RawHandle {
    /// Partition descriptor from the image's partition table.
    pub descriptor: PartitionDescriptor,
    read_fn: Box<dyn Fn() -> crate::Result<Vec<u8>> + Send + Sync>,
}

impl RawHandle {
    /// Create a new [`RawHandle`].
    pub fn new(
        descriptor: PartitionDescriptor,
        read_fn: impl Fn() -> crate::Result<Vec<u8>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            descriptor,
            read_fn: Box::new(read_fn),
        }
    }

    /// Read the raw bytes of this partition.
    pub fn read_raw(&self) -> crate::Result<Vec<u8>> {
        (self.read_fn)()
    }
}

/// Handle to the MBR boot-code area (bytes 0–439 of the raw disk).
///
/// This region precedes the partition table at offset 446 and contains
/// the GRUB stage-1 jump stub (or any other bootloader stage-1 code).
/// It is not a real partition-table entry; it is represented as a
/// synthetic partition with number 0 and kind [`PartitionKind::MbrBootCode`].
///
/// [`PartitionKind::MbrBootCode`]: crate::partition::PartitionKind::MbrBootCode
pub struct MbrHandle {
    /// Synthetic descriptor (number = 0, kind = MbrBootCode).
    pub descriptor: PartitionDescriptor,
    read_fn: Box<dyn Fn() -> crate::Result<Vec<u8>> + Send + Sync>,
}

impl MbrHandle {
    /// Create a new [`MbrHandle`].
    pub fn new(
        descriptor: PartitionDescriptor,
        read_fn: impl Fn() -> crate::Result<Vec<u8>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            descriptor,
            read_fn: Box::new(read_fn),
        }
    }

    /// Read the 440 bytes of the MBR boot-code area.
    pub fn read_raw(&self) -> crate::Result<Vec<u8>> {
        (self.read_fn)()
    }
}

/// A partition accessible through an open image, in one of three forms.
pub enum PartitionHandle {
    /// BIOS-boot (raw bytes, e.g. GRUB stage 1.5).
    BiosBoot(BiosBootHandle),
    /// Formatted filesystem partition.
    Fs(FsHandle),
    /// Unformatted raw partition.
    Raw(RawHandle),
    /// MBR boot-code area (bytes 0–439, before partition table at offset 446).
    /// Synthetic partition number 0; not a real partition-table entry.
    Mbr(MbrHandle),
}

impl PartitionHandle {
    /// Return the [`PartitionDescriptor`] regardless of variant.
    pub fn descriptor(&self) -> &PartitionDescriptor {
        match self {
            Self::BiosBoot(h) => &h.descriptor,
            Self::Fs(h) => &h.descriptor,
            Self::Raw(h) => &h.descriptor,
            Self::Mbr(h) => &h.descriptor,
        }
    }
}

// ── OpenImage trait ───────────────────────────────────────────────────────────

/// A successfully opened image whose partition structure is known.
///
/// Returned by [`Image::open`].  Implementations hold open any OS-level
/// resources (e.g. an NBD connection) for the duration of the object's
/// lifetime.
pub trait OpenImage: Send + Sync {
    /// The disk partition layout as parsed from the image's partition table.
    fn disk_layout(&self) -> &DiskLayout;

    /// Return one [`PartitionHandle`] per partition in the image.
    ///
    /// For [`DiskScheme::SingleFs`] images (plain directories) this returns a
    /// single [`PartitionHandle::Fs`].
    ///
    /// For qcow2 images the list is prepended with a [`PartitionHandle::Mbr`]
    /// (synthetic number 0) whenever the raw disk could be read at open time,
    /// so that the MBR boot-code area is compressed alongside the real partitions.
    fn partitions(&self) -> crate::Result<Vec<PartitionHandle>>;
}

// ── Image trait ───────────────────────────────────────────────────────────────

/// Abstraction over image container formats.
///
/// # Implementors
///
/// - [`DirectoryImage`]: a plain directory; no mounting needed.  Used in tests
///   and for providers that already extract images to directories.
/// - `Qcow2Image` (behind `feature = "qcow2"`): mounts via `qemu-nbd`.
///
/// [`DirectoryImage`]: crate::DirectoryImage
pub trait Image: Send + Sync {
    /// Short name stored in the manifest header (`"directory"`, `"qcow2"`, …).
    fn format_name(&self) -> &'static str;

    /// Open the image at `path` and parse its partition layout.
    ///
    /// Returns an [`OpenImage`] that keeps any OS-level resources alive until
    /// dropped.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Format`] if the image cannot be opened or parsed.
    fn open(&self, path: &Path) -> crate::Result<Box<dyn OpenImage>>;

    /// Make the filesystem contents of a single-partition image accessible as
    /// a directory root (legacy API, used by older tests and the old scheduler).
    ///
    /// New code should use [`open`][Image::open] instead.
    fn mount(&self, path: &Path) -> crate::Result<Box<dyn MountHandle>>;

    /// Pack the filesystem tree rooted at `source_dir` into a container at
    /// `output_path` (legacy directory-copy API).
    fn pack(&self, source_dir: &Path, output_path: &Path) -> crate::Result<()>;
}

// ── OpenDirectory (DirectoryImage OpenImage impl) ────────────────────────────

/// [`OpenImage`] returned by `DirectoryImage::open()`.
///
/// The directory itself is the single filesystem partition; no actual mounting
/// is performed.
pub struct OpenDirectory {
    path: PathBuf,
    layout: DiskLayout,
}

impl OpenDirectory {
    pub(crate) fn new(path: PathBuf) -> Self {
        let layout = DiskLayout {
            scheme: DiskScheme::SingleFs,
            disk_guid: None,
            partitions: vec![],
        };
        Self { path, layout }
    }
}

impl OpenImage for OpenDirectory {
    fn disk_layout(&self) -> &DiskLayout {
        &self.layout
    }

    fn partitions(&self) -> crate::Result<Vec<PartitionHandle>> {
        use crate::partition::PartitionKind;

        let descriptor = PartitionDescriptor {
            number: 1,
            partition_guid: None,
            type_guid: None,
            name: Some("root".into()),
            start_lba: 0,
            end_lba: 0,
            size_bytes: 0,
            flags: 0,
            kind: PartitionKind::Fs {
                fs_type: "directory".into(),
            },
        };

        let path = self.path.clone();
        let handle = FsHandle::new(descriptor, move || {
            Ok(Box::new(SimpleMountHandle::new(path.clone())))
        });

        Ok(vec![PartitionHandle::Fs(handle)])
    }
}
