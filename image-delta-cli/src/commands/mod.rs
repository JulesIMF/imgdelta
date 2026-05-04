// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// CLI subcommand registry

pub mod compress;
pub mod decompress;
pub mod image;
pub mod manifest;

#[cfg(debug_assertions)]
pub mod debug;

pub use image::ImageCommands;
pub use manifest::ManifestCommands;

#[cfg(debug_assertions)]
pub use debug::DebugCommands;
