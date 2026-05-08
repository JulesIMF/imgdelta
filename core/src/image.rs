// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Image and OpenImage traits

use std::path::{Path, PathBuf};

use crate::partition::{DiskLayout, DiskScheme, PartitionDescriptor};
use crate::partitions::{FsHandle, MountHandle, PartitionHandle, SimpleMountHandle};

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
