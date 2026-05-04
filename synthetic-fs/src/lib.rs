// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// image-delta-synthetic-fs: synthetic filesystem image generator and mutator

//! # image-delta-synthetic-fs
//!
//! A standalone crate for generating and mutating synthetic filesystem trees.
//!
//! Designed to be used as a test helper for delta-compression pipelines, but
//! contains no dependencies on `image-delta-core` or `image-delta-cli` and can
//! be reused in other projects.
//!
//! ## Quick start
//!
//! ```rust
//! use image_delta_synthetic_fs::{FsTreeBuilder, FsMutator, MutationConfig};
//! use rand::SeedableRng;
//! use rand::rngs::StdRng;
//!
//! // Build a random initial image.
//! let mut tree = FsTreeBuilder::new(42).build();
//! assert!(tree.len() >= 20);
//!
//! // Apply one round of mutations.
//! let mut rng = StdRng::seed_from_u64(1);
//! let log = FsMutator::new(MutationConfig::default()).mutate(&mut tree, &mut rng);
//! assert!(!log.is_empty());
//! ```

pub mod builder;
pub mod fstree;
pub mod mutator;

pub use builder::FsTreeBuilder;
pub use fstree::{EntryMeta, FsEntry, FsTree};
pub use mutator::{FsMutator, ModKind, MutationConfig, MutationKind, MutationLog, MutationRecord};
