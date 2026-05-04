// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// debug walkdir: walk a directory tree and print entry metadata

use std::path::PathBuf;

use anyhow::Context;
use clap::Args;
use image_delta_core::fs_diff::{diff_dirs, DiffKind, TreeStats};

#[derive(Args, Debug)]
pub struct WalkdirArgs {
    /// Old (base) filesystem root to compare from.
    pub old_path: PathBuf,

    /// New (target) filesystem root to compare against.
    pub new_path: PathBuf,

    /// Print MetadataOnly entries (mode/uid/gid/mtime changes) in addition to
    /// content changes.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    pub show_metadata: bool,
}

pub fn run(args: WalkdirArgs) -> anyhow::Result<()> {
    let result = diff_dirs(&args.old_path, &args.new_path).with_context(|| {
        format!(
            "comparing '{}' vs '{}'",
            args.old_path.display(),
            args.new_path.display(),
        )
    })?;

    let mut n_added: usize = 0;
    let mut n_removed: usize = 0;
    let mut n_changed: usize = 0;
    let mut n_metadata: usize = 0;

    // Sort by path so the output is stable (walkdir order is OS-dependent).
    let mut diffs = result.diffs;
    diffs.sort_by(|a, b| a.path.cmp(&b.path));

    for diff in &diffs {
        match diff.kind {
            DiffKind::Added => {
                println!("+ {}", diff.path);
                n_added += 1;
            }
            DiffKind::Removed => {
                println!("- {}", diff.path);
                n_removed += 1;
            }
            DiffKind::Changed => {
                println!("~ {}", diff.path);
                n_changed += 1;
            }
            DiffKind::MetadataOnly => {
                if args.show_metadata {
                    println!("M {}", diff.path);
                }
                n_metadata += 1;
            }
        }
    }

    // ── Diff summary ─────────────────────────────────────────────────────────
    let total = n_added + n_removed + n_changed + n_metadata;
    println!();
    println!("─── diff summary ────────────────────────");
    println!("  +  added:         {n_added}");
    println!("  -  removed:       {n_removed}");
    println!("  ~  changed:       {n_changed}");
    println!("  M  metadata-only: {n_metadata}");
    println!("  ─────────────────────────────────────────");
    println!("     total diffs:   {total}");

    // ── Tree stats ────────────────────────────────────────────────────────────
    println!();
    println!("─── tree stats ──────────────────────────");
    print_tree_stats("old (base)  ", &result.base);
    print_tree_stats("new (target)", &result.target);

    if total == 0 {
        println!();
        println!("(directories are identical)");
    }

    Ok(())
}

fn print_tree_stats(label: &str, s: &TreeStats) {
    println!(
        "  {label}  {:>6} files  {:>10}  {:>5} dirs  {:>5} symlinks",
        s.files,
        fmt_bytes(s.total_bytes),
        s.dirs,
        s.symlinks,
    );
}

fn fmt_bytes(n: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    const GIB: u64 = 1024 * MIB;
    if n >= GIB {
        format!("{:.1} GiB", n as f64 / GIB as f64)
    } else if n >= MIB {
        format!("{:.1} MiB", n as f64 / MIB as f64)
    } else if n >= KIB {
        format!("{:.1} KiB", n as f64 / KIB as f64)
    } else {
        format!("{n} B")
    }
}
