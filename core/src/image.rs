// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Image and OpenImage traits

use std::path::Path;

use crate::manifest::PartitionManifest;
use crate::partition::DiskLayout;
use crate::partitions::{MountHandle, PartitionHandle};

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

    /// Create (or overwrite) one partition in a writable image and return a
    /// writable handle to it.
    ///
    /// The default implementation returns [`crate::Error::Format`] with
    /// `"create_partition not supported"`, so read-only image backends do not
    /// need to override this method.
    ///
    /// The full [`PartitionManifest`] is passed (not just the descriptor) so
    /// that implementations can access `fs_type`, `fs_uuid`, etc. from the
    /// partition content to run `mkfs` or similar setup before returning the
    /// handle.
    fn create_partition(&self, _pm: &PartitionManifest) -> crate::Result<PartitionHandle> {
        Err(crate::Error::Format(
            "create_partition not supported for this image format".into(),
        ))
    }
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

    /// Create a new, empty, writable image at `path` with the given partition
    /// layout and return an [`OpenImage`] that supports [`OpenImage::create_partition`].
    ///
    /// For qcow2: runs `qemu-img create`, connects NBD RW, writes the GPT.
    /// For directory: calls `create_dir_all(path)`.
    ///
    /// The default implementation returns [`crate::Error::Format`].
    fn create(&self, _path: &Path, _layout: &DiskLayout) -> crate::Result<Box<dyn OpenImage>> {
        Err(crate::Error::Format(
            "create not supported for this image format".into(),
        ))
    }
}
