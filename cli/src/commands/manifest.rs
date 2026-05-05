// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// manifest subcommand: inspect and diff image manifests

use clap::{Args, Subcommand};
use std::collections::{HashMap, HashSet};
use std::path::Path;

use image_delta_core::manifest::{Data, Manifest, PartitionContent, Patch, Record};

use crate::commands::compress::load_config;

#[derive(Subcommand, Debug)]
pub enum ManifestCommands {
    /// Display manifest contents for an image.
    Inspect(InspectArgs),
    /// Compare the file sets recorded in two image manifests.
    Diff(DiffArgs),
}

#[derive(Args, Debug)]
pub struct InspectArgs {
    /// Image ID whose manifest to display.
    #[arg(value_name = "IMAGE_ID")]
    pub image_id: String,

    /// Output format.
    #[arg(long, default_value = "table", value_parser = ["table", "json"])]
    pub format: String,

    /// Show per-file details for each Fs partition.
    #[arg(long, short = 'f')]
    pub files: bool,
}

#[derive(Args, Debug)]
pub struct DiffArgs {
    /// First image ID.
    #[arg(value_name = "IMAGE_ID_A")]
    pub image_a: String,

    /// Second image ID.
    #[arg(value_name = "IMAGE_ID_B")]
    pub image_b: String,
}

pub async fn run(cmd: ManifestCommands, config_path: Option<&Path>) -> anyhow::Result<()> {
    match cmd {
        ManifestCommands::Inspect(args) => inspect(args, config_path).await,
        ManifestCommands::Diff(args) => diff(args, config_path).await,
    }
}

async fn inspect(args: InspectArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    let config = load_config(config_path)?;
    let storage = config.storage.build().await?;

    let bytes = storage
        .download_manifest(&args.image_id)
        .await
        .map_err(|e| anyhow::anyhow!("download_manifest '{}': {e}", args.image_id))?;

    let manifest = Manifest::from_bytes(&bytes)
        .map_err(|e| anyhow::anyhow!("parse manifest '{}': {e}", args.image_id))?;

    if args.format == "json" {
        println!("{}", serde_json::to_string_pretty(&manifest)?);
        return Ok(());
    }

    // Table format
    let h = &manifest.header;
    println!("image_id:      {}", h.image_id);
    println!(
        "base_image_id: {}",
        h.base_image_id.as_deref().unwrap_or("-")
    );
    println!("format:        {}", h.format);
    println!("version:       {}", h.version);
    println!("partitions:    {}", manifest.partitions.len());
    println!();

    for pm in &manifest.partitions {
        let desc = &pm.descriptor;
        match &pm.content {
            PartitionContent::BiosBoot { size, .. } => {
                println!("  partition {}  kind=bios_boot  size={size}", desc.number);
            }
            PartitionContent::Raw { size, blob, patch } => {
                let stored = if blob.is_some() {
                    "blob"
                } else if patch.is_some() {
                    "patch"
                } else {
                    "empty"
                };
                println!(
                    "  partition {}  kind=raw  size={size}  stored={stored}",
                    desc.number
                );
            }
            PartitionContent::Fs { fs_type, records } => {
                let n_added = records
                    .iter()
                    .filter(|r| r.old_path.is_none() && r.new_path.is_some())
                    .count();
                let n_removed = records
                    .iter()
                    .filter(|r| r.new_path.is_none() && r.old_path.is_some())
                    .count();
                let n_renamed = records
                    .iter()
                    .filter(|r| {
                        r.old_path.is_some() && r.new_path.is_some() && r.old_path != r.new_path
                    })
                    .count();
                let n_patched = records
                    .iter()
                    .filter(|r| {
                        r.old_path.is_some()
                            && r.new_path.is_some()
                            && r.old_path == r.new_path
                            && (r.data.is_some() || r.patch.is_some())
                    })
                    .count();
                let n_meta_only = records
                    .iter()
                    .filter(|r| {
                        r.old_path.is_some()
                            && r.new_path.is_some()
                            && r.data.is_none()
                            && r.patch.is_none()
                            && r.old_path == r.new_path
                            && r.metadata.is_some()
                    })
                    .count();

                let total_source_bytes: u64 = records.iter().map(|r| r.size).sum();

                println!(
                    "  partition {}  kind=fs  fs_type={fs_type}  records={}",
                    desc.number,
                    records.len(),
                );
                println!(
                    "    added={n_added}  removed={n_removed}  patched={n_patched}  \
                     renamed={n_renamed}  meta_only={n_meta_only}"
                );
                println!("    source_bytes={total_source_bytes}");

                if args.files || records.len() <= 50 {
                    println!();
                    println!("    {:<4}  {:<12}  {:<7}  path", "op", "size", "stored");
                    println!("    {}", "-".repeat(60));
                    let mut sorted: Vec<&Record> = records.iter().collect();
                    sorted.sort_by_key(|r| {
                        r.new_path
                            .as_deref()
                            .or(r.old_path.as_deref())
                            .unwrap_or("")
                    });
                    for r in sorted {
                        println!("    {}", format_record(r));
                    }
                    println!();
                }
            }
        }
    }
    Ok(())
}

async fn diff(args: DiffArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    let config = load_config(config_path)?;
    let storage = config.storage.build().await?;

    let fetch = |id: String| {
        let storage = Arc::clone(&storage);
        async move {
            let bytes = storage
                .download_manifest(&id)
                .await
                .map_err(|e| anyhow::anyhow!("download_manifest '{id}': {e}"))?;
            Manifest::from_bytes(&bytes).map_err(|e| anyhow::anyhow!("parse manifest '{id}': {e}"))
        }
    };

    let (ma, mb) = tokio::try_join!(fetch(args.image_a.clone()), fetch(args.image_b.clone()))?;

    println!("diff {} → {}", args.image_a, args.image_b);
    println!();

    // Build path→record maps for each manifest's Fs partitions.
    // We clone records to avoid lifetime issues with the async fetch closures.
    let fs_map = |manifest: &Manifest| -> HashMap<u32, HashMap<String, Record>> {
        let mut out: HashMap<u32, HashMap<String, Record>> = HashMap::new();
        for pm in &manifest.partitions {
            if let PartitionContent::Fs { records, .. } = &pm.content {
                let m = out.entry(pm.descriptor.number).or_default();
                for r in records {
                    if let Some(p) = r.new_path.as_deref().or(r.old_path.as_deref()) {
                        m.insert(p.to_string(), r.clone());
                    }
                }
            }
        }
        out
    };

    let maps_a = fs_map(&ma);
    let maps_b = fs_map(&mb);

    // Union of partition numbers.
    let part_nums: HashSet<u32> = maps_a.keys().chain(maps_b.keys()).copied().collect();

    let mut any = false;
    for pnum in {
        let mut v: Vec<u32> = part_nums.into_iter().collect();
        v.sort();
        v
    } {
        let empty = HashMap::new();
        let pa = maps_a.get(&pnum).unwrap_or(&empty);
        let pb = maps_b.get(&pnum).unwrap_or(&empty);

        let all_paths: HashSet<&str> = pa
            .keys()
            .map(|s| s.as_str())
            .chain(pb.keys().map(|s| s.as_str()))
            .collect();

        let mut only_a: Vec<&str> = Vec::new();
        let mut only_b: Vec<&str> = Vec::new();
        let mut different: Vec<&str> = Vec::new();
        let mut same: usize = 0;

        for path in &all_paths {
            match (pa.get(*path), pb.get(*path)) {
                (Some(ra), Some(rb)) => {
                    if ra.data != rb.data || ra.patch != rb.patch || ra.metadata != rb.metadata {
                        different.push(path);
                    } else {
                        same += 1;
                    }
                }
                (Some(_), None) => only_a.push(path),
                (None, Some(_)) => only_b.push(path),
                (None, None) => unreachable!(),
            }
        }

        only_a.sort_unstable();
        only_b.sort_unstable();
        different.sort_unstable();

        if only_a.is_empty() && only_b.is_empty() && different.is_empty() {
            println!("  partition {pnum}: identical ({same} common records)");
            continue;
        }
        any = true;
        println!("  partition {pnum}:");
        for p in &only_a {
            println!("    < {p}   (only in {})", args.image_a);
        }
        for p in &only_b {
            println!("    > {p}   (only in {})", args.image_b);
        }
        for p in &different {
            println!("    ~ {p}   (differs)");
        }
        if same > 0 {
            println!("    … {same} record(s) identical");
        }
    }

    if !any {
        println!("  (manifests record identical file-level changes)");
    }

    Ok(())
}

fn format_record(r: &Record) -> String {
    let is_rename = matches!((&r.old_path, &r.new_path), (Some(op), Some(np)) if op != np);

    let (op, path) = match (&r.old_path, &r.new_path) {
        (None, Some(np)) => ("+", np.as_str()),
        (Some(op), None) => ("-", op.as_str()),
        (Some(_op), Some(np)) if is_rename => ("R", np.as_str()),
        (Some(_op), Some(np)) if r.data.is_some() || r.patch.is_some() => ("M", np.as_str()),
        (Some(op), Some(np)) if op == np => ("=", np.as_str()),
        (Some(_op), Some(np)) => ("?", np.as_str()),
        (None, None) => ("?", ""),
    };

    let stored = match (&r.patch, &r.data) {
        (Some(Patch::Real(_)), _) => "patch".to_string(),
        (_, Some(Data::BlobRef(b))) => format!("blob({}B)", b.size),
        (_, Some(Data::SoftlinkTo(_))) => "symlink".to_string(),
        (_, Some(Data::HardlinkTo(_))) => "hardlink".to_string(),
        (_, Some(Data::SpecialDevice(_))) => "special".to_string(),
        _ => "-".to_string(),
    };

    let path_display = if is_rename {
        format!(
            "{} → {}",
            r.old_path.as_deref().unwrap_or(""),
            r.new_path.as_deref().unwrap_or("")
        )
    } else {
        path.to_string()
    };

    format!(
        "{op:<4}  {size:<12}  {stored:<14}  {path_display}",
        size = r.size,
    )
}

// Need Arc for the fetch closure.
use std::sync::Arc;
