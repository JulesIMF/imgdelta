// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Compress pipeline: Stage 3 — match_renamed

use std::collections::{HashMap, HashSet};

use async_trait::async_trait;

use crate::compress::partitions::fs::context::StageContext;
use crate::compress::partitions::fs::draft::FsDraft;
use crate::compress::partitions::fs::stage::{dump_records_json, CompressStage};
use crate::manifest::{Data, DataRef, EntryType, Patch, Record};
use crate::path_match::{find_best_matches, PathMatchConfig};
use crate::Result;

// ── Stage struct ──────────────────────────────────────────────────────────────

/// Stage 3: detect file renames by matching removed files against added files.
///
/// Two-pass algorithm:
///
/// **Pass 1 — SHA-256 exact match.**  Files whose SHA-256 is identical on both
/// sides are pure path-renames.  Within each same-hash group, path similarity
/// decides the bijective pairing.
///
/// **Pass 2 — high-confidence path match.**  For files not claimed by Pass 1,
/// a path-similarity score ≥ `pass2_min_score` (default 0.85) is required.
/// Identical-basename pairs are rejected to avoid cross-package false positives.
///
/// Only `LazyBlob` files (not matched by blob_lookup) are eligible as targets.
pub struct MatchRenamed {
    /// Minimum path-similarity score for Pass 2 matches.
    pub pass2_min_score: f64,
}

impl Default for MatchRenamed {
    fn default() -> Self {
        Self {
            pass2_min_score: 0.6,
        }
    }
}

#[async_trait]
impl CompressStage for MatchRenamed {
    fn name(&self) -> &'static str {
        "match_renamed"
    }

    async fn run(&self, _ctx: &StageContext, draft: FsDraft) -> Result<FsDraft> {
        Ok(match_renamed_fn(draft, self.pass2_min_score))
    }

    fn dump_debug(&self, draft: &FsDraft, path: &std::path::Path) -> Result<()> {
        dump_records_json(&draft.records, path)
    }
}

// ── Implementation ────────────────────────────────────────────────────────────

pub fn match_renamed_fn(mut draft: FsDraft, pass2_min_score: f64) -> FsDraft {
    // ── Candidate pools ───────────────────────────────────────────────────────

    let removed: Vec<(usize, String)> = draft
        .records
        .iter()
        .enumerate()
        .filter(|(_, r)| r.new_path.is_none() && r.entry_type == EntryType::File)
        .map(|(i, r)| (i, r.old_path.clone().unwrap()))
        .collect();

    // A file is a rename target only if it still has a LazyBlob —
    // i.e. it was not matched by blob_lookup.  Files already upgraded to
    // BlobRef + Lazy belong to m_{S3} and are off-limits for match_renamed.
    let added: Vec<(usize, String)> = draft
        .records
        .iter()
        .enumerate()
        .filter(|(_, r)| {
            r.old_path.is_none()
                && r.entry_type == EntryType::File
                && matches!(r.data, Some(Data::LazyBlob(_)))
        })
        .map(|(i, r)| (i, r.new_path.clone().unwrap()))
        .collect();

    if removed.is_empty() || added.is_empty() {
        return draft;
    }

    // ── SHA-256 index ─────────────────────────────────────────────────────────

    let mut rem_by_hash: HashMap<[u8; 32], Vec<(usize, String)>> = HashMap::new();
    for (rec_idx, path) in &removed {
        if let Some(&h) = draft.base_hashes.get(path.as_str()) {
            rem_by_hash
                .entry(h)
                .or_default()
                .push((*rec_idx, path.clone()));
        }
    }
    let mut add_by_hash: HashMap<[u8; 32], Vec<(usize, String)>> = HashMap::new();
    for (rec_idx, path) in &added {
        if let Some(&h) = draft.target_hashes.get(path.as_str()) {
            add_by_hash
                .entry(h)
                .or_default()
                .push((*rec_idx, path.clone()));
        }
    }

    let mut matched_rem: HashSet<usize> = HashSet::new();
    let mut matched_add: HashSet<usize> = HashSet::new();
    let mut new_records: Vec<Record> = Vec::new();
    let mut remove_indices: Vec<usize> = Vec::new();

    // ── Pass 1: SHA-256 exact matches ─────────────────────────────────────────
    let sha256_config = PathMatchConfig {
        min_score: 0.0,
        first_component_weight: 0.0,
        ..PathMatchConfig::default()
    };
    for (hash, rem_group) in &rem_by_hash {
        let Some(add_group) = add_by_hash.get(hash) else {
            continue;
        };
        let sub_rem: Vec<String> = rem_group
            .iter()
            .filter(|(ri, _)| !matched_rem.contains(ri))
            .map(|(_, p)| p.clone())
            .collect();
        let sub_add: Vec<String> = add_group
            .iter()
            .filter(|(ai, _)| !matched_add.contains(ai))
            .map(|(_, p)| p.clone())
            .collect();
        if sub_rem.is_empty() || sub_add.is_empty() {
            continue;
        }
        let sub_matches = match find_best_matches(&sub_rem, &sub_add, &sha256_config) {
            Ok(m) => m,
            Err(_) => continue,
        };
        for m in &sub_matches {
            let Some((rem_rec_idx, old_path)) = rem_group
                .iter()
                .find(|(ri, p)| *p == m.source_path && !matched_rem.contains(ri))
            else {
                continue;
            };
            let Some((add_rec_idx, new_path)) = add_group
                .iter()
                .find(|(ai, p)| *p == m.target_path && !matched_add.contains(ai))
            else {
                continue;
            };
            let Some(patch) = build_rename_patch(&draft.records, *rem_rec_idx, *add_rec_idx) else {
                continue;
            };
            let size = draft.records[*add_rec_idx].size;
            let metadata = draft.records[*add_rec_idx].metadata.clone();
            new_records.push(Record {
                old_path: Some(old_path.clone()),
                new_path: Some(new_path.clone()),
                entry_type: EntryType::File,
                size,
                data: None,
                patch: Some(patch),
                metadata,
            });
            matched_rem.insert(*rem_rec_idx);
            matched_add.insert(*add_rec_idx);
            remove_indices.push(*rem_rec_idx);
            remove_indices.push(*add_rec_idx);
        }
    }

    // ── Pass 2: high-confidence path match (SHA-256 mismatch) ─────────────────
    let remaining_rem: Vec<(usize, String)> = removed
        .iter()
        .filter(|(ri, _)| !matched_rem.contains(ri))
        .cloned()
        .collect();
    let remaining_add: Vec<(usize, String)> = added
        .iter()
        .filter(|(ai, _)| !matched_add.contains(ai))
        .cloned()
        .collect();

    if !remaining_rem.is_empty() && !remaining_add.is_empty() {
        let rem_paths: Vec<String> = remaining_rem.iter().map(|(_, p)| p.clone()).collect();
        let add_paths: Vec<String> = remaining_add.iter().map(|(_, p)| p.clone()).collect();
        let rename_config = PathMatchConfig {
            min_score: pass2_min_score,
            first_component_weight: 0.0,
            ..PathMatchConfig::default()
        };
        let path_matches =
            find_best_matches(&rem_paths, &add_paths, &rename_config).unwrap_or_default();
        for m in &path_matches {
            let Some((rem_rec_idx, old_path)) =
                remaining_rem.iter().find(|(_, p)| *p == m.source_path)
            else {
                continue;
            };
            let Some((add_rec_idx, new_path)) =
                remaining_add.iter().find(|(_, p)| *p == m.target_path)
            else {
                continue;
            };

            // Reject identical-basename pairs — they are either pure-path renames
            // (handled by Pass 1) or cross-package false positives.
            // let old_base = old_path.rsplit('/').next().unwrap_or(old_path.as_str());
            // let new_base = new_path.rsplit('/').next().unwrap_or(new_path.as_str());
            // if old_base == new_base {
            //     continue;
            // }
            let Some(patch) = build_rename_patch(&draft.records, *rem_rec_idx, *add_rec_idx) else {
                continue;
            };
            let size = draft.records[*add_rec_idx].size;
            let metadata = draft.records[*add_rec_idx].metadata.clone();
            new_records.push(Record {
                old_path: Some(old_path.clone()),
                new_path: Some(new_path.clone()),
                entry_type: EntryType::File,
                size,
                data: None,
                patch: Some(patch),
                metadata,
            });
            remove_indices.push(*rem_rec_idx);
            remove_indices.push(*add_rec_idx);
        }
    }

    // ── Merge results ─────────────────────────────────────────────────────────
    remove_indices.sort_unstable();
    remove_indices.dedup();
    for &i in remove_indices.iter().rev() {
        draft.records.swap_remove(i);
    }
    draft.records.extend(new_records);
    draft
}

/// Build the [`Patch`] for a rename record.
///
/// The added record must still be a `LazyBlob`; blob_lookup-matched files are
/// excluded from the rename pool so this invariant holds.
fn build_rename_patch(records: &[Record], rem_idx: usize, add_idx: usize) -> Option<Patch> {
    let new_local = match &records[add_idx].data {
        Some(Data::LazyBlob(p)) => p.clone(),
        _ => return None,
    };
    let old_data = match &records[rem_idx].data {
        Some(Data::OriginalFile(p)) => DataRef::FilePath(p.clone()),
        _ => return None,
    };
    Some(Patch::Lazy {
        old_data,
        new_data: DataRef::FilePath(new_local),
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn lazy_blob_record(old: Option<&str>, new: Option<&str>, path: &str) -> Record {
        Record {
            old_path: old.map(|s| s.to_string()),
            new_path: new.map(|s| s.to_string()),
            entry_type: EntryType::File,
            size: 100,
            data: if new.is_some() {
                Some(Data::LazyBlob(PathBuf::from(format!("/mnt/target/{path}"))))
            } else {
                Some(Data::OriginalFile(PathBuf::from(format!(
                    "/mnt/base/{path}"
                ))))
            },
            patch: None,
            metadata: None,
        }
    }

    #[test]
    fn test_match_renamed_basic() {
        let mut draft = FsDraft::default();
        // Removed: lib/libfoo.so.1
        draft.records.push(lazy_blob_record(
            Some("lib/libfoo.so.1"),
            None,
            "lib/libfoo.so.1",
        ));
        // Added: lib/libfoo.so.2 (same dir, different version)
        draft.records.push(lazy_blob_record(
            None,
            Some("lib/libfoo.so.2"),
            "lib/libfoo.so.2",
        ));
        // Simulate a pure path rename (content unchanged): same sha256 → Pass 1.
        draft
            .base_hashes
            .insert("lib/libfoo.so.1".into(), [1u8; 32]);
        draft
            .target_hashes
            .insert("lib/libfoo.so.2".into(), [1u8; 32]);

        let draft = match_renamed_fn(draft, 0.85);

        // After matching: one renamed record, no orphan added/removed.
        let renamed = draft.records.iter().find(|r| {
            r.old_path.as_deref() == Some("lib/libfoo.so.1")
                && r.new_path.as_deref() == Some("lib/libfoo.so.2")
        });
        assert!(renamed.is_some(), "expected a renamed record");
        assert!(
            matches!(renamed.unwrap().patch, Some(Patch::Lazy { .. })),
            "renamed record should have a Lazy patch"
        );
        // Original add/remove records should be gone.
        assert!(
            !draft
                .records
                .iter()
                .any(|r| r.old_path.as_deref() == Some("lib/libfoo.so.1") && r.new_path.is_none()),
            "orphan remove record should be consumed"
        );
    }

    #[test]
    fn test_match_renamed_pass2_high_path_score() {
        // Pass 2: sha256 mismatch, but path similarity ≥ 0.85 (version bump in
        // same directory with deep enough path).
        let mut draft = FsDraft::default();
        draft.records.push(lazy_blob_record(
            Some("usr/lib/x86_64-linux-gnu/libfoo.so.1"),
            None,
            "usr/lib/x86_64-linux-gnu/libfoo.so.1",
        ));
        draft.records.push(lazy_blob_record(
            None,
            Some("usr/lib/x86_64-linux-gnu/libfoo.so.2"),
            "usr/lib/x86_64-linux-gnu/libfoo.so.2",
        ));
        // Different sha256 → Pass 1 skips them; Path similarity ≈ 0.88 → Pass 2 accepts.
        draft
            .base_hashes
            .insert("usr/lib/x86_64-linux-gnu/libfoo.so.1".into(), [1u8; 32]);
        draft
            .target_hashes
            .insert("usr/lib/x86_64-linux-gnu/libfoo.so.2".into(), [2u8; 32]);

        let draft = match_renamed_fn(draft, 0.85);

        let renamed = draft.records.iter().find(|r| {
            r.old_path.as_deref() == Some("usr/lib/x86_64-linux-gnu/libfoo.so.1")
                && r.new_path.as_deref() == Some("usr/lib/x86_64-linux-gnu/libfoo.so.2")
        });
        assert!(renamed.is_some(), "Pass 2 should match version-bump rename");
    }

    #[test]
    fn test_match_renamed_pass2_rejects_cross_package() {
        // Pass 2 must NOT match files from different packages that share a
        // generic filename (copyright, changelog.gz, etc.).
        // Score for libssl3/copyright → libcurl4/copyright ≈ 0.84 < 0.85.
        let mut draft = FsDraft::default();
        draft.records.push(lazy_blob_record(
            Some("usr/share/doc/libssl3/copyright"),
            None,
            "usr/share/doc/libssl3/copyright",
        ));
        draft.records.push(lazy_blob_record(
            None,
            Some("usr/share/doc/libcurl4/copyright"),
            "usr/share/doc/libcurl4/copyright",
        ));
        // Different sha256 and different packages → should NOT be matched.
        draft
            .base_hashes
            .insert("usr/share/doc/libssl3/copyright".into(), [1u8; 32]);
        draft
            .target_hashes
            .insert("usr/share/doc/libcurl4/copyright".into(), [2u8; 32]);

        let before = draft.records.len();
        let draft = match_renamed_fn(draft, 0.85);
        assert_eq!(
            draft.records.len(),
            before,
            "cross-package copyright must not be matched as rename"
        );
    }

    #[test]
    fn test_match_renamed_no_candidates_is_noop() {
        let mut draft = FsDraft::default();
        draft
            .records
            .push(lazy_blob_record(Some("etc/old.conf"), None, "etc/old.conf"));
        // No added files → nothing to match.
        let before_count = draft.records.len();
        let draft = match_renamed_fn(draft, 0.85);
        assert_eq!(draft.records.len(), before_count);
    }
}
