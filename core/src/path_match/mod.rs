// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Path matching utilities used by the RouterEncoder glob rules

use std::collections::{HashMap, HashSet};

use rayon::prelude::*;

// ── Public types ──────────────────────────────────────────────────────────────

/// A scored correspondence between a path in the base image and a path in the
/// target image.
#[derive(Debug, Clone)]
pub struct PathMatch {
    /// Path in the base image (the "removed" side of the diff).
    pub source_path: String,
    /// Path in the target image (the "added" side of the diff).
    pub target_path: String,
    /// Similarity score in `[0.0, 1.0]`.  Higher is a better match.
    pub score: f64,
}

/// Tuning parameters for the path-matching algorithm.
#[derive(Debug, Clone)]
pub struct PathMatchConfig {
    /// Matches below this score are discarded.
    pub min_score: f64,
    /// Cost of substituting one digit for another (vs cost=1.0 for letters).
    /// Default 0.3 makes version changes (`libc-2.31` → `libc-2.35`) cheap.
    pub digit_weight: f64,
    /// Multiplier applied when the first path component differs.
    /// A larger value penalises cross-directory matches more.
    pub first_component_weight: f64,
    /// Penalty weight for depth mismatch between two paths.
    pub depth_penalty: f64,
}

impl Default for PathMatchConfig {
    fn default() -> Self {
        Self {
            min_score: 0.5,
            digit_weight: 0.3,
            first_component_weight: 5.0,
            depth_penalty: 0.3,
        }
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Find the best base-image path for each target-image path.
///
/// Returns one [`PathMatch`] per target path that has a sufficiently similar
/// base counterpart (`score ≥ config.min_score`).  Matching is a **bijection**:
/// each base path is assigned to at most one target path and vice-versa.
///
/// The algorithm:
/// 1. Build fast lookup indexes on `base_paths` (by extension, first component,
///    path-length bucket).
/// 2. For each target path, score a filtered candidate set.
/// 3. Collect all `(target, base, score)` triples that exceed the threshold.
/// 4. Sort descending by score and greedily pick non-conflicting pairs.
///
/// Target paths with no good match are omitted — those files will be stored
/// without a delta base.
///
/// # Errors
///
/// Currently infallible; always returns `Ok`.  The `Result` wrapper exists for
/// future extension (e.g. loading a pre-computed index from disk).
pub fn find_best_matches(
    source_paths: &[String],
    target_paths: &[String],
    config: &PathMatchConfig,
) -> crate::Result<Vec<PathMatch>> {
    if source_paths.is_empty() || target_paths.is_empty() {
        return Ok(Vec::new());
    }

    // Build fast indexes on source_paths.
    let (ext_index, first_comp_index, length_index): (ExtIndex, ExtIndex, LengthIndex) =
        build_indexes(source_paths);

    // Phase 1: score candidates for every target path in parallel.
    // Each target path is independent — we collect per-target vecs and flatten.
    // The indexes are read-only after construction and thus safe to share.
    let all_candidates: Vec<(usize, usize, f64)> = target_paths
        .par_iter()
        .enumerate()
        .flat_map(|(t_idx, target_path)| {
            let candidate_base_indices = get_candidates(
                target_path,
                &ext_index,
                &first_comp_index,
                &length_index,
                source_paths,
            );

            let max_edit_dist = target_path.len().max(200) as f64 * 0.3;
            let t_first = target_path.split('/').next().unwrap_or("");
            let t_ext = ext_of(target_path);

            let mut local: Vec<(usize, usize, f64)> = Vec::new();
            for b_idx in candidate_base_indices {
                let base_path = &source_paths[b_idx];

                // Fast pre-filter: skip if lengths are wildly different.
                let len_ratio_ok = {
                    let tl = target_path.len() as f64;
                    let bl = base_path.len() as f64;
                    (tl - bl).abs() <= tl.max(bl) * 0.4
                };
                if !len_ratio_ok {
                    continue;
                }

                // Fast pre-filter: skip if both first component AND extension differ.
                let b_first = base_path.split('/').next().unwrap_or("");
                let b_ext = ext_of(base_path);
                if t_first != b_first && t_ext != b_ext {
                    continue;
                }

                let score = path_similarity(
                    target_path,
                    base_path,
                    config.digit_weight,
                    config.first_component_weight,
                    config.depth_penalty,
                    Some(max_edit_dist),
                );

                if score >= config.min_score {
                    local.push((t_idx, b_idx, score));
                }
            }
            local
        })
        .collect();

    // Phase 2: greedy bijective selection (best score first).
    let mut all_candidates = all_candidates;
    all_candidates.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

    let mut matched_target: HashSet<usize> = HashSet::new();
    let mut matched_base: HashSet<usize> = HashSet::new();
    let mut results: Vec<PathMatch> = Vec::new();

    for (t_idx, b_idx, score) in all_candidates {
        if matched_target.contains(&t_idx) || matched_base.contains(&b_idx) {
            continue;
        }
        matched_target.insert(t_idx);
        matched_base.insert(b_idx);
        results.push(PathMatch {
            source_path: source_paths[b_idx].clone(),
            target_path: target_paths[t_idx].clone(),
            score,
        });
    }

    Ok(results)
}

// ── Internal: indexes ─────────────────────────────────────────────────────────

/// Build three lookup indexes over `paths`.
///
/// Returns `(ext_index, first_comp_index, length_index)` where each maps a
/// key to the set of indices in `paths` that share that key.
type ExtIndex = HashMap<String, HashSet<usize>>;
type LengthIndex = HashMap<usize, HashSet<usize>>;

#[allow(clippy::type_complexity)]
fn build_indexes(paths: &[String]) -> (ExtIndex, ExtIndex, LengthIndex) {
    let mut ext_index: HashMap<String, HashSet<usize>> = HashMap::new();
    let mut first_comp_index: HashMap<String, HashSet<usize>> = HashMap::new();
    let mut length_index: HashMap<usize, HashSet<usize>> = HashMap::new();

    for (idx, path) in paths.iter().enumerate() {
        ext_index.entry(ext_of(path)).or_default().insert(idx);

        let first = path.split('/').next().unwrap_or("").to_string();
        first_comp_index.entry(first).or_default().insert(idx);

        let bucket = path.len() / 10;
        length_index.entry(bucket).or_default().insert(idx);
    }

    (ext_index, first_comp_index, length_index)
}

/// Return a filtered set of base-path indices that are plausible matches for
/// `target_path`, using the pre-built indexes.
fn get_candidates(
    target_path: &str,
    ext_index: &HashMap<String, HashSet<usize>>,
    first_comp_index: &HashMap<String, HashSet<usize>>,
    length_index: &HashMap<usize, HashSet<usize>>,
    source_paths: &[String],
) -> Vec<usize> {
    let t_ext = ext_of(target_path);
    let t_first = target_path.split('/').next().unwrap_or("").to_string();
    let center_bucket = target_path.len() / 10;

    let ext_cands = ext_index.get(&t_ext).cloned().unwrap_or_default();
    let first_cands = first_comp_index.get(&t_first).cloned().unwrap_or_default();

    let mut length_cands = HashSet::new();
    for bucket in center_bucket.saturating_sub(2)..=center_bucket + 2 {
        if let Some(set) = length_index.get(&bucket) {
            length_cands.extend(set);
        }
    }

    // Intersection strategy: try to narrow down aggressively.
    let candidates: HashSet<usize> = if !ext_cands.is_empty() {
        let inter: HashSet<_> = ext_cands.intersection(&first_cands).copied().collect();
        if !inter.is_empty() {
            let inter2: HashSet<_> = inter.intersection(&length_cands).copied().collect();
            if !inter2.is_empty() {
                inter2
            } else {
                inter
            }
        } else {
            let inter2: HashSet<_> = ext_cands.intersection(&length_cands).copied().collect();
            if !inter2.is_empty() {
                inter2
            } else {
                ext_cands
            }
        }
    } else if !first_cands.is_empty() {
        let inter: HashSet<_> = first_cands.intersection(&length_cands).copied().collect();
        if !inter.is_empty() {
            inter
        } else {
            first_cands
        }
    } else {
        length_cands
    };

    // Limit to MAX_CANDIDATES, prioritised by filename similarity.
    let mut cands_vec: Vec<usize> = candidates.into_iter().collect();
    const MAX_CANDIDATES: usize = 500;
    if cands_vec.len() > MAX_CANDIDATES {
        let t_filename = target_path.split('/').next_back().unwrap_or("");
        cands_vec.sort_by_cached_key(|&idx| {
            let b_filename = source_paths[idx].split('/').next_back().unwrap_or("");
            let d = levenshtein(t_filename, b_filename, 0.3, None);
            (d * 1000.0) as i64
        });
        cands_vec.truncate(MAX_CANDIDATES);
    }

    cands_vec
}

// ── Internal: similarity scoring ──────────────────────────────────────────────

/// Compute a `[0.0, 1.0]` similarity score between two paths.
///
/// Combines:
/// - Weighted Levenshtein edit similarity on the full path string.
/// - Fraction of matching path components in the same position.
/// - Position-weighted component matching score.
/// - Penalties for: depth mismatch, different first component, filename diff.
fn path_similarity(
    p1: &str,
    p2: &str,
    digit_weight: f64,
    first_component_weight: f64,
    depth_penalty_weight: f64,
    max_edit_distance: Option<f64>,
) -> f64 {
    let parts1: Vec<&str> = p1.split('/').filter(|s| !s.is_empty()).collect();
    let parts2: Vec<&str> = p2.split('/').filter(|s| !s.is_empty()).collect();

    let len1 = parts1.len();
    let len2 = parts2.len();
    let total = len1.max(len2);

    // Depth penalty.
    let depth_diff = (len1 as i32 - len2 as i32).unsigned_abs() as f64;
    let depth_penalty = if total > 0 {
        (depth_diff / total as f64) * depth_penalty_weight
    } else {
        0.0
    };

    // First-component penalty.
    let first_penalty = if !parts1.is_empty() && !parts2.is_empty() && parts1[0] != parts2[0] {
        first_component_weight * 0.1
    } else {
        0.0
    };

    // Filename penalty.
    let fname1 = parts1.last().unwrap_or(&"");
    let fname2 = parts2.last().unwrap_or(&"");
    let filename_penalty = if fname1 != fname2 {
        let d = levenshtein(fname1, fname2, 1.0, None);
        let max_len = fname1.len().max(fname2.len()) as f64;
        if max_len > 0.0 {
            (d / max_len) * 0.15
        } else {
            0.0
        }
    } else {
        0.0
    };

    // Position-weighted component matching.
    let min_len = len1.min(len2);
    let mut common_parts = 0;
    let mut pos_weighted = 0.0_f64;

    for i in 0..min_len {
        if parts1[i] == parts2[i] {
            common_parts += 1;
            pos_weighted += 1.0 / ((i + 1) as f64).sqrt();
        } else if i > 0 && i < 3 {
            pos_weighted -= 0.3 / (i + 1) as f64;
        }
    }

    let max_weight: f64 = (0..min_len).map(|i| 1.0 / ((i + 1) as f64).sqrt()).sum();
    let pos_match_score = if max_weight > 0.0 {
        pos_weighted / max_weight
    } else {
        0.0
    };

    // Edit distance similarity.
    let edit_dist = levenshtein(p1, p2, digit_weight, max_edit_distance);
    if edit_dist == f64::INFINITY {
        return 0.0;
    }
    let max_len = p1.len().max(p2.len());
    let edit_sim = if max_len == 0 {
        1.0
    } else {
        1.0 - (edit_dist / max_len as f64)
    };

    let comp_ratio = if total > 0 {
        common_parts as f64 / total as f64
    } else {
        0.0
    };

    let score = edit_sim * 0.5 + comp_ratio * 0.3 + pos_match_score * 0.2;
    (score - depth_penalty - first_penalty - filename_penalty).clamp(0.0, 1.0)
}

/// Weighted Levenshtein distance.
///
/// Substitutions between two digits cost `digit_weight` instead of 1.0.
/// Substitutions between a digit and a letter cost 0.7.
///
/// Returns `f64::INFINITY` when the edit distance exceeds `max_distance`.
fn levenshtein(s1: &str, s2: &str, digit_weight: f64, max_distance: Option<f64>) -> f64 {
    // Ensure s1 is the longer string for memory efficiency.
    if s1.len() < s2.len() {
        return levenshtein(s2, s1, digit_weight, max_distance);
    }

    let len2 = s2.len();
    if len2 == 0 {
        return s1.len() as f64;
    }

    if let Some(max) = max_distance {
        if (s1.len() as i64 - len2 as i64).unsigned_abs() as f64 > max {
            return f64::INFINITY;
        }
    }

    let s1_chars: Vec<char> = s1.chars().collect();
    let s2_chars: Vec<char> = s2.chars().collect();
    let mut prev: Vec<f64> = (0..=len2).map(|i| i as f64).collect();

    for (i, &c1) in s1_chars.iter().enumerate() {
        let mut cur = vec![i as f64 + 1.0];
        let mut row_min = i as f64 + 1.0;

        for (j, &c2) in s2_chars.iter().enumerate() {
            let cost = if c1 == c2 {
                0.0
            } else if c1.is_ascii_digit() && c2.is_ascii_digit() {
                digit_weight
            } else if c1.is_ascii_digit() || c2.is_ascii_digit() {
                0.7
            } else {
                1.0
            };

            let val = (prev[j + 1] + 1.0).min(cur[j] + 1.0).min(prev[j] + cost);
            cur.push(val);
            if val < row_min {
                row_min = val;
            }
        }

        if let Some(max) = max_distance {
            if row_min > max {
                return f64::INFINITY;
            }
        }

        prev = cur;
    }

    prev[len2]
}

/// File extension of a path string, or `""` if absent.
fn ext_of(path: &str) -> String {
    std::path::Path::new(path)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string()
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> PathMatchConfig {
        PathMatchConfig::default()
    }

    fn paths(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    // 1. Exact path match → score 1.0 and bijection is 1:1.
    #[test]
    fn test_exact_match() {
        let base = paths(&["/usr/bin/bash"]);
        let target = paths(&["/usr/bin/bash"]);
        let result = find_best_matches(&base, &target, &cfg()).unwrap();
        assert_eq!(result.len(), 1);
        let m = &result[0];
        assert_eq!(m.source_path, "/usr/bin/bash");
        assert_eq!(m.target_path, "/usr/bin/bash");
        assert!(
            (m.score - 1.0).abs() < 1e-9,
            "score should be 1.0, got {}",
            m.score
        );
    }

    // 2. Version rename: only digits differ → high score (digit_weight = 0.3).
    #[test]
    fn test_version_rename() {
        let base = paths(&["lib/libc-2.31.so"]);
        let target = paths(&["lib/libc-2.35.so"]);
        let result = find_best_matches(&base, &target, &cfg()).unwrap();
        assert_eq!(result.len(), 1, "should match version rename");
        assert!(
            result[0].score > 0.7,
            "version rename should have high score ({})",
            result[0].score
        );
    }

    // 3. Different first component → penalty → lower score.
    #[test]
    fn test_different_dir_low_score() {
        let base = paths(&["usr/bin/foo"]);
        let target = paths(&["opt/bar/foo"]);
        let mut strict = cfg();
        strict.min_score = 0.0; // no threshold so we see the actual score
        let result = find_best_matches(&base, &target, &strict).unwrap();
        // May or may not match depending on score; if it does, score should be penalised.
        if !result.is_empty() {
            assert!(
                result[0].score < 0.95,
                "cross-directory match should be penalised (score {})",
                result[0].score
            );
        }
    }

    // 4. Completely different paths → no match above default threshold.
    #[test]
    fn test_no_match_below_threshold() {
        let base = paths(&["usr/bin/ls"]);
        let target = paths(&["var/log/syslog.1.gz"]);
        let result = find_best_matches(&base, &target, &cfg()).unwrap();
        assert!(
            result.is_empty(),
            "unrelated paths should not match: {:?}",
            result
        );
    }

    // 5. N:N bijection — each removed file has exactly one clear counterpart.
    #[test]
    fn test_one_to_one_bijection() {
        let base = paths(&[
            "lib/libfoo-1.0.so",
            "lib/libbar-1.0.so",
            "lib/libbaz-1.0.so",
        ]);
        let target = paths(&[
            "lib/libfoo-2.0.so",
            "lib/libbar-2.0.so",
            "lib/libbaz-2.0.so",
        ]);
        let result = find_best_matches(&base, &target, &cfg()).unwrap();
        assert_eq!(result.len(), 3, "expected 3 bijective matches");

        // No base_path or target_path may appear twice.
        let unique_base: HashSet<_> = result.iter().map(|m| &m.source_path).collect();
        let unique_target: HashSet<_> = result.iter().map(|m| &m.target_path).collect();
        assert_eq!(unique_base.len(), 3, "duplicate base paths");
        assert_eq!(unique_target.len(), 3, "duplicate target paths");
    }

    // 6. Empty inputs → no match, no panic.
    #[test]
    fn test_empty_inputs() {
        assert!(find_best_matches(&[], &[], &cfg()).unwrap().is_empty());
        assert!(find_best_matches(&paths(&["a/b"]), &[], &cfg())
            .unwrap()
            .is_empty());
        assert!(find_best_matches(&[], &paths(&["a/b"]), &cfg())
            .unwrap()
            .is_empty());
    }

    // 7. Extensionless binaries — no ext_index hit, similarity is filename-only.
    //    usr/bin/python3.9 → usr/bin/python3.10 must still match.
    #[test]
    fn test_extensionless_binaries() {
        let base = paths(&["usr/bin/python3.9", "usr/bin/ruby2.7", "usr/bin/perl5.30"]);
        let target = paths(&["usr/bin/python3.10", "usr/bin/ruby3.0", "usr/bin/perl5.36"]);
        let result = find_best_matches(&base, &target, &cfg()).unwrap();
        // At minimum, python3.9 → python3.10 should be found (clear version bump)
        let python_match = result.iter().find(|m| {
            m.source_path == "usr/bin/python3.9" && m.target_path == "usr/bin/python3.10"
        });
        assert!(
            python_match.is_some(),
            "python3.9 → python3.10 not matched; results: {result:?}"
        );
    }

    // 8. Score ordering — results must be sorted by descending score.
    #[test]
    fn test_score_ordering() {
        let base = paths(&["lib/libssl-1.1.1.so", "lib/libcrypto-1.1.1.so"]);
        let target = paths(&["lib/libssl-3.0.0.so", "lib/libcrypto-3.0.0.so"]);
        let result = find_best_matches(&base, &target, &cfg()).unwrap();
        assert!(!result.is_empty(), "expected at least one match");
        for w in result.windows(2) {
            assert!(
                w[0].score >= w[1].score,
                "results not sorted: {:.3} < {:.3}",
                w[0].score,
                w[1].score
            );
        }
    }

    // 9. Asymmetric: many removed, few added → at most |added| matches (bijection).
    #[test]
    fn test_asymmetric_many_removed_few_added() {
        // 10 removed kernel modules, only 2 corresponding added ones.
        let base = paths(&[
            "lib/modules/5.10.0-10/kernel/drivers/net/e1000.ko",
            "lib/modules/5.10.0-10/kernel/drivers/net/e1000e.ko",
            "lib/modules/5.10.0-10/kernel/drivers/net/igb.ko",
            "lib/modules/5.10.0-10/kernel/drivers/net/ixgbe.ko",
            "lib/modules/5.10.0-10/kernel/drivers/net/tg3.ko",
            "lib/modules/5.10.0-10/kernel/fs/btrfs.ko",
            "lib/modules/5.10.0-10/kernel/fs/ext4.ko",
            "lib/modules/5.10.0-10/kernel/fs/xfs.ko",
            "lib/modules/5.10.0-10/kernel/crypto/aes.ko",
            "lib/modules/5.10.0-10/kernel/crypto/sha256.ko",
        ]);
        let target = paths(&[
            "lib/modules/5.10.0-19/kernel/drivers/net/e1000.ko",
            "lib/modules/5.10.0-19/kernel/fs/btrfs.ko",
        ]);
        let result = find_best_matches(&base, &target, &cfg()).unwrap();
        assert!(
            result.len() <= target.len(),
            "more matches ({}) than target paths ({})",
            result.len(),
            target.len()
        );
        // Bijection: no target path appears twice
        let unique: HashSet<_> = result.iter().map(|m| &m.target_path).collect();
        assert_eq!(
            unique.len(),
            result.len(),
            "target_path duplicate in bijection"
        );
    }

    // 10. Depth penalty — identical filename but different depth should score lower.
    #[test]
    fn test_depth_penalty_works() {
        // target is at same depth as base → high score
        // target is much deeper than base → lower score
        let base = paths(&["usr/lib/libfoo-1.0.so"]);
        let shallow = paths(&["usr/lib/libfoo-2.0.so"]);
        let deep = paths(&["usr/lib/x86_64-linux-gnu/gconv/nested/libfoo-2.0.so"]);

        let r_shallow = find_best_matches(&base, &shallow, &cfg()).unwrap();
        let r_deep = find_best_matches(&base, &deep, &cfg()).unwrap();

        let score_shallow = r_shallow.first().map(|m| m.score).unwrap_or(0.0);
        let score_deep = r_deep.first().map(|m| m.score).unwrap_or(0.0);
        assert!(
            score_shallow > score_deep,
            "shallow score ({score_shallow:.3}) should exceed deep score ({score_deep:.3})"
        );
    }

    // 11. Rename + new blob candidates: simulate the case where a file is both
    //     renamed AND has new content (→ manifest entry gets blob + metadata.new_path).
    //
    //     fs_diff produces Added(new_path) and Removed(old_path).
    //     path_match must identify these as rename candidates so the compressor
    //     can store: manifest_entry { blob: Some(blob_ref), metadata: { new_path: old_path } }
    //     instead of treating them as two independent operations (full blob + tombstone).
    #[test]
    fn test_blob_rename_candidates_detected() {
        // Simulate a kernel package update: modules removed at old version path,
        // new blobs added at new version path — content is also new (hence "blob").
        let removed_blobs = paths(&[
            "usr/lib/modules/5.15.0-91-generic/kernel/net/ipv4/tcp_bbr.ko",
            "usr/lib/modules/5.15.0-91-generic/kernel/drivers/gpu/drm/i915/i915.ko",
            "usr/lib/modules/5.15.0-91-generic/kernel/drivers/net/wireless/iwlwifi/iwlwifi.ko",
        ]);
        let added_blobs = paths(&[
            "usr/lib/modules/5.15.0-105-generic/kernel/net/ipv4/tcp_bbr.ko",
            "usr/lib/modules/5.15.0-105-generic/kernel/drivers/gpu/drm/i915/i915.ko",
            "usr/lib/modules/5.15.0-105-generic/kernel/drivers/net/wireless/iwlwifi/iwlwifi.ko",
        ]);
        let result = find_best_matches(&removed_blobs, &added_blobs, &cfg()).unwrap();

        // All three should be identified as rename candidates.
        assert_eq!(
            result.len(),
            3,
            "expected 3 rename+blob candidates, got {}: {result:?}",
            result.len()
        );

        // Each score should be high (clear version-number rename pattern).
        for m in &result {
            assert!(
                m.score >= 0.7,
                "blob rename candidate score too low: {:.3} ({} → {})",
                m.score,
                m.source_path,
                m.target_path
            );
        }

        // The compressor would store these as blob+rename, not add+remove.
        // Verify bijection (no double-assignment).
        let seen_base: HashSet<_> = result.iter().map(|m| &m.source_path).collect();
        let seen_target: HashSet<_> = result.iter().map(|m| &m.target_path).collect();
        assert_eq!(seen_base.len(), 3);
        assert_eq!(seen_target.len(), 3);
    }

    // 12. No false positives — completely unrelated files must not be matched.
    #[test]
    fn test_no_false_positives_unrelated_files() {
        let base = paths(&[
            "etc/dhcp/dhclient.conf",
            "var/spool/cron/crontabs/root",
            "home/user/.bashrc",
        ]);
        let target = paths(&[
            "usr/lib/modules/5.10.0-19/kernel/drivers/gpu/drm/amdgpu/amdgpu.ko",
            "boot/vmlinuz-5.10.0-19-amd64",
            "usr/share/locale/fr/LC_MESSAGES/libc.mo",
        ]);
        // With default min_score=0.5, none of these should match.
        let result = find_best_matches(&base, &target, &cfg()).unwrap();
        assert!(
            result.is_empty(),
            "got unexpected false-positive matches: {result:?}"
        );
    }

    // ── Fixture-based tests ───────────────────────────────────────────────────

    /// Load the embedded fixture pair (base_paths / target_paths) and run
    /// the matcher.  These paths come from a real debian-11 image pair
    /// (`private/results/v2_unpacking/debian-11/pair_1_14/delta_manifest.json`).
    fn load_fixture() -> (Vec<String>, Vec<String>) {
        let base: Vec<String> =
            serde_json::from_str(include_str!("fixtures/synthetic/base_paths.json")).unwrap();
        let target: Vec<String> =
            serde_json::from_str(include_str!("fixtures/synthetic/target_paths.json")).unwrap();
        (base, target)
    }

    // 7. Fixture: matcher runs without panicking and returns results in [0,1].
    #[test]
    fn test_fixture_no_panic_scores_valid() {
        let (base, target) = load_fixture();
        let result = find_best_matches(&base, &target, &cfg()).unwrap();
        for m in &result {
            assert!(
                (0.0..=1.0).contains(&m.score),
                "score out of range: {} (match {:?} → {:?})",
                m.score,
                m.source_path,
                m.target_path
            );
        }
    }

    // 8. Fixture: bijection invariant — each path appears at most once.
    #[test]
    fn test_fixture_bijection() {
        let (base, target) = load_fixture();
        let result = find_best_matches(&base, &target, &cfg()).unwrap();

        let mut seen_base: HashSet<&str> = HashSet::new();
        let mut seen_target: HashSet<&str> = HashSet::new();
        for m in &result {
            assert!(
                seen_base.insert(m.source_path.as_str()),
                "duplicate base_path: {}",
                m.source_path
            );
            assert!(
                seen_target.insert(m.target_path.as_str()),
                "duplicate target_path: {}",
                m.target_path
            );
        }
    }

    // 9. Fixture: at least 50 % of removed files (base_paths) are matched.
    //    This is a sanity check that the algorithm is actually finding renames,
    //    not returning empty results due to threshold being too strict.
    #[test]
    fn test_fixture_match_rate_acceptable() {
        let (base, target) = load_fixture();
        let n_base = base.len();
        let result = find_best_matches(&base, &target, &cfg()).unwrap();
        let match_rate = result.len() as f64 / n_base as f64;
        assert!(
            match_rate >= 0.5,
            "match rate {:.1}% is below 50% — algorithm may be broken",
            match_rate * 100.0
        );
    }

    // ── Real-world dump fixtures ──────────────────────────────────────────────

    fn run_real_fixture(name: &str, base: &[String], target: &[String]) -> f64 {
        let result = find_best_matches(base, target, &cfg()).unwrap();

        // Bijection invariant.
        let mut seen_base = std::collections::HashSet::new();
        let mut seen_target = std::collections::HashSet::new();
        for m in &result {
            assert!(
                seen_base.insert(m.source_path.as_str()),
                "[{name}] duplicate base_path: {}",
                m.source_path
            );
            assert!(
                seen_target.insert(m.target_path.as_str()),
                "[{name}] duplicate target_path: {}",
                m.target_path
            );
            assert!(
                (0.0..=1.0).contains(&m.score),
                "[{name}] score out of range: {}",
                m.score
            );
        }

        let match_rate = result.len() as f64 / base.len().max(1) as f64;
        eprintln!(
            "[{name}] base={}, target={}, matched={}, rate={:.1}%",
            base.len(),
            target.len(),
            result.len(),
            match_rate * 100.0
        );
        if !result.is_empty() {
            eprintln!(
                "  top match: {:?} → {:?} (score={:.3})",
                result[0].source_path, result[0].target_path, result[0].score
            );
        }
        match_rate
    }

    // 10. CentOS pair 1: kernel module renames (4.18.0-517 → 4.18.0-522).
    //     These are clear version-bump renames — expect high match rate.
    #[test]
    fn test_real_centos_pair1_kernel_renames() {
        let base: Vec<String> =
            serde_json::from_str(include_str!("fixtures/centos_pair1/base_paths.json")).unwrap();
        let target: Vec<String> =
            serde_json::from_str(include_str!("fixtures/centos_pair1/target_paths.json")).unwrap();
        let rate = run_real_fixture("centos_pair1", &base, &target);
        assert!(
            rate >= 0.5,
            "centos_pair1 match rate {:.1}% < 50%",
            rate * 100.0
        );
    }

    // 11. CentOS pair 2: kernel module renames (4.18.0-536 → 4.18.0-546).
    #[test]
    fn test_real_centos_pair2_kernel_renames() {
        let base: Vec<String> =
            serde_json::from_str(include_str!("fixtures/centos_pair2/base_paths.json")).unwrap();
        let target: Vec<String> =
            serde_json::from_str(include_str!("fixtures/centos_pair2/target_paths.json")).unwrap();
        let rate = run_real_fixture("centos_pair2", &base, &target);
        assert!(
            rate >= 0.5,
            "centos_pair2 match rate {:.1}% < 50%",
            rate * 100.0
        );
    }

    // 12. Debian pair 1: cloud-instance directory rename (random UUID → new UUID).
    //     Files have identical names but live in different UUID-named directories.
    //     Algorithm may or may not match them — we only check bijection / no panic.
    #[test]
    fn test_real_debian_pair1_cloud_instances() {
        let base: Vec<String> =
            serde_json::from_str(include_str!("fixtures/debian_pair1/base_paths.json")).unwrap();
        let target: Vec<String> =
            serde_json::from_str(include_str!("fixtures/debian_pair1/target_paths.json")).unwrap();
        run_real_fixture("debian_pair1", &base, &target);
        // No match-rate assertion: UUIDs differ, outcome is informational.
    }

    // 13. Debian pair 2: shared-library version renames (libbind9-9.16.48 → 9.16.50).
    //     Clear version-bump pattern — expect high match rate.
    #[test]
    fn test_real_debian_pair2_library_renames() {
        let base: Vec<String> =
            serde_json::from_str(include_str!("fixtures/debian_pair2/base_paths.json")).unwrap();
        let target: Vec<String> =
            serde_json::from_str(include_str!("fixtures/debian_pair2/target_paths.json")).unwrap();
        let rate = run_real_fixture("debian_pair2", &base, &target);
        assert!(
            rate >= 0.5,
            "debian_pair2 match rate {:.1}% < 50%",
            rate * 100.0
        );
    }
}
