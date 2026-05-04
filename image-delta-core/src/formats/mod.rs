// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Re-exports all Image format implementations (directory, qcow2)

pub mod directory;
#[cfg(all(target_os = "linux", feature = "qcow2"))]
pub mod qcow2;

pub use directory::DirectoryImage;
#[cfg(all(target_os = "linux", feature = "qcow2"))]
pub use qcow2::Qcow2Image;
