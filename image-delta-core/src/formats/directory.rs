use crate::{format::SimpleMountHandle, ImageFormat, MountHandle, Result};
use std::path::Path;
use walkdir::WalkDir;

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

    fn pack(&self, source_dir: &Path, output_path: &Path) -> Result<()> {
        if output_path.exists() {
            std::fs::remove_dir_all(output_path)
                .map_err(|e| crate::Error::Format(e.to_string()))?;
        }
        std::fs::create_dir_all(output_path).map_err(|e| crate::Error::Format(e.to_string()))?;
        for entry in WalkDir::new(source_dir).min_depth(1) {
            let entry = entry.map_err(|e| crate::Error::Format(e.to_string()))?;
            let rel = entry
                .path()
                .strip_prefix(source_dir)
                .map_err(|e| crate::Error::Format(e.to_string()))?;
            let dest = output_path.join(rel);
            if entry.file_type().is_dir() {
                std::fs::create_dir_all(&dest).map_err(|e| crate::Error::Format(e.to_string()))?;
            } else {
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| crate::Error::Format(e.to_string()))?;
                }
                std::fs::copy(entry.path(), &dest)
                    .map_err(|e| crate::Error::Format(e.to_string()))?;
            }
        }
        Ok(())
    }
}
