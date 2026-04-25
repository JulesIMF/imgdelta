use crate::{format::SimpleMountHandle, ImageFormat, MountHandle, Result};
use std::path::Path;

/// [`ImageFormat`] implementation for a plain directory.
///
/// "Mounting" is a no-op: the directory is used as-is.  No external tools
/// required.  This is the format used for all L1 and L2 tests, and for
/// providers whose pipeline already extracts images to directories.
pub struct DirectoryFormat;

impl DirectoryFormat {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DirectoryFormat {
    fn default() -> Self {
        Self::new()
    }
}

impl ImageFormat for DirectoryFormat {
    fn format_name(&self) -> &'static str {
        "directory"
    }

    fn mount(&self, path: &Path) -> Result<Box<dyn MountHandle>> {
        Ok(Box::new(SimpleMountHandle::new(path.to_path_buf())))
    }
}
