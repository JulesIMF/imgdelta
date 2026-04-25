use std::path::{Path, PathBuf};

/// RAII handle to a mounted or otherwise accessible filesystem root.
///
/// Dropping the handle must unmount/clean up any associated resources
/// (e.g. detach qemu-nbd device, remove temp directory).
pub trait MountHandle: Send {
    /// Path to the directory that is the root of the mounted filesystem.
    fn root(&self) -> &Path;
}

/// Abstraction over image container formats.
///
/// The scheduler and compressor only ever see a [`MountHandle::root`] directory
/// and never deal with qcow2 internals, tar archives, or other container formats.
///
/// # Implementors
///
/// - [`DirectoryFormat`]: a plain directory; no mounting needed. Used in tests.
/// - `Qcow2Format` (behind `feature = "qcow2"`): mounts via `qemu-nbd`.
///
/// [`DirectoryFormat`]: crate::DirectoryFormat
pub trait ImageFormat: Send + Sync {
    /// Short name stored in the manifest header.
    fn format_name(&self) -> &'static str;

    /// Make the filesystem contents accessible under a returned directory root.
    ///
    /// The returned handle must keep any OS-level resources alive until dropped.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Format`] if mounting fails.
    fn mount(&self, path: &Path) -> crate::Result<Box<dyn MountHandle>>;
}

/// Simple [`MountHandle`] that just wraps a [`PathBuf`].  Used by
/// [`DirectoryFormat`] and in tests.
///
/// [`DirectoryFormat`]: crate::DirectoryFormat
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
