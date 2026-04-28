pub mod directory;
#[cfg(all(target_os = "linux", feature = "qcow2"))]
pub mod qcow2;

pub use directory::DirectoryImage;
#[cfg(all(target_os = "linux", feature = "qcow2"))]
pub use qcow2::Qcow2Image;
