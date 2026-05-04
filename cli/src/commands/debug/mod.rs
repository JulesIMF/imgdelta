// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// debug subcommand group: low-level diagnostic tools

pub mod walkdir_cmd;

use clap::Subcommand;

/// Debug-only subcommands — compiled only in debug builds (`cargo build`).
///
/// These commands expose internal algorithms for interactive inspection
/// against real data.  They are **not** present in release builds.
#[derive(Subcommand, Debug)]
pub enum DebugCommands {
    /// Walk two directory trees with `diff_dirs` and print per-file diff markers.
    ///
    /// Output lines are prefixed with:
    ///   `+`  added in NEW_PATH
    ///   `-`  removed (present in OLD_PATH, absent in NEW_PATH)
    ///   `~`  content changed
    ///   `M`  metadata-only change (mode, uid, gid, or mtime)
    Walkdir(walkdir_cmd::WalkdirArgs),
}

pub fn run(cmd: DebugCommands) -> anyhow::Result<()> {
    match cmd {
        DebugCommands::Walkdir(args) => walkdir_cmd::run(args),
    }
}
