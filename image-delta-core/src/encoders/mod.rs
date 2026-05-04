// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Re-exports all built-in PatchEncoder implementations

pub mod passthrough;
pub mod text_diff;
pub mod vcdiff;

pub use passthrough::PassthroughEncoder;
pub use text_diff::TextDiffEncoder;
pub use vcdiff::Xdelta3Encoder;
