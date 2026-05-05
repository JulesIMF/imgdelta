// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Stress tests: N_ITERATIONS × N_CHAINS random compress/decompress chain round-trips.

mod common;

use std::collections::VecDeque;

use common::{
    compare_dirs, compress_opts_workers, decompress_opts_workers, make_compressor,
    save_root_meta_for_storage, verify_manifest_records, CategoryMetrics, ManifestCheckResult,
};
use image_delta_core::Compressor;
use image_delta_core::{Manifest, PartitionContent};
use image_delta_synthetic_fs::mutator::{MutationConfig, MutationLog};
use image_delta_synthetic_fs::{FsMutator, FsTreeBuilder};
use indicatif::{ProgressBar, ProgressStyle};
use rand::rngs::StdRng;
use rand::Rng;
use rand::RngCore;
use rand::SeedableRng;

// ── constants (defaults, overridable via env) ─────────────────────────────────

/// Number of outer iterations (each iteration creates N_CHAINS independent families).
const N_ITERATIONS: usize = 20;

/// Number of independent chain families per iteration.
const N_CHAINS: usize = 3;

/// Number of rayon workers used by each compress/decompress call in the parallel stress variant.
const N_PAR_WORKERS: usize = 4;

/// Length of each mutation chain (delta images after the base).
const CHAIN_LENGTH: usize = 5;

// ── runtime config ────────────────────────────────────────────────────────────

/// Stress-test parameters, resolved once from environment variables with
/// the compile-time constants as defaults.
///
/// Override via:
///   STRESS_SEED=12345 STRESS_N_ITERATIONS=50 STRESS_N_CHAINS=5 STRESS_CHAIN_LENGTH=8 \
///     cargo test -p image-delta-core --test stress_chain -- --nocapture
///
/// `STRESS_SEED` pins the base RNG seed for full reproducibility.  When omitted
/// a fresh random seed is drawn from the OS and printed at test start so that
/// any failing run can be replayed by re-setting `STRESS_SEED`.
#[derive(Clone, Copy)]
struct StressConfig {
    n_iterations: usize,
    n_chains: usize,
    chain_length: usize,
    /// Master seed that all per-(iter,chain) seeds are derived from.
    base_seed: u64,
}

impl StressConfig {
    fn from_env() -> Self {
        let parse = |name: &str, default: usize| -> usize {
            std::env::var(name)
                .ok()
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(default)
        };
        let base_seed: u64 = std::env::var("STRESS_SEED")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or_else(|| rand::thread_rng().next_u64());
        Self {
            n_iterations: parse("STRESS_N_ITERATIONS", N_ITERATIONS),
            n_chains: parse("STRESS_N_CHAINS", N_CHAINS),
            chain_length: parse("STRESS_CHAIN_LENGTH", CHAIN_LENGTH),
            base_seed,
        }
    }
}

// ── per-chain state ───────────────────────────────────────────────────────────

struct ChainState {
    /// image_id of the base (unmodified) image.
    base_id: String,
    /// Temporary directory holding the base fs snapshot on disk.
    base_dir: tempfile::TempDir,
    /// image_id for each generation [0..CHAIN_LENGTH).
    gen_ids: Vec<String>,
    /// Temporary directories holding gen[i] snapshot on disk.
    gen_dirs: Vec<tempfile::TempDir>,
    /// Mutation log for each generation: logs[i] = changes from gen[i-1] (or base) to gen[i].
    mut_logs: Vec<MutationLog>,
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Build the base image and mutation chain for one family.
///
/// Compresses the base image into `storage` via `compressor`, then writes all
/// mutated snapshots to disk.  Returns the chain state ready for delta
/// compression in any order.
async fn build_chain(
    iter: usize,
    chain: usize,
    cfg: StressConfig,
    workers: usize,
    compressor: &image_delta_core::DefaultCompressor,
) -> ChainState {
    // Unique seed per (iter, chain), derived from the master seed.
    let seed = cfg.base_seed
        ^ (iter as u64).wrapping_mul(0x_9e37_79b9)
        ^ ((chain as u64) << 32)
        ^ 0x_dead_cafe_u64;
    let base_id = format!("stress-base-i{iter}-c{chain}");
    let base_dir = tempfile::tempdir().unwrap();

    let base_tree = FsTreeBuilder::new(seed).with_hardlinks(false).build();
    base_tree.write_to_dir(base_dir.path()).unwrap();

    // Compress base against empty dir.
    let empty = tempfile::tempdir().unwrap();
    compressor
        .compress(
            empty.path(),
            base_dir.path(),
            compress_opts_workers(&base_id, None, workers),
        )
        .await
        .unwrap();

    // Mutate cfg.chain_length times.
    let mutator = FsMutator::new(MutationConfig {
        // Hardlinks disabled: copy_unchanged_from_base copies them as
        // independent files, breaking the nlink > 1 check in compare_dirs.
        weight_add_hardlink: 0,
        ..MutationConfig::default()
    });
    let mut current_tree = base_tree;
    let mut gen_ids = Vec::with_capacity(cfg.chain_length);
    let mut gen_dirs = Vec::with_capacity(cfg.chain_length);
    let mut mut_logs = Vec::with_capacity(cfg.chain_length);

    for gen in 0..cfg.chain_length {
        let gen_seed = seed ^ ((gen as u64).wrapping_mul(0x_6c62_272e));
        let mut rng = StdRng::seed_from_u64(gen_seed);
        let log = mutator.mutate(&mut current_tree, &mut rng);

        let gen_dir = tempfile::tempdir().unwrap();
        current_tree.write_to_dir(gen_dir.path()).unwrap();

        gen_ids.push(format!("stress-i{iter}-c{chain}-g{gen}"));
        gen_dirs.push(gen_dir);
        mut_logs.push(log);
    }

    ChainState {
        base_id,
        base_dir,
        gen_ids,
        gen_dirs,
        mut_logs,
    }
}

/// Extract fs records from a directory-format manifest.
fn fs_records(manifest: &Manifest) -> &[image_delta_core::Record] {
    for pm in &manifest.partitions {
        if let PartitionContent::Fs { records, .. } = &pm.content {
            return records;
        }
    }
    &[]
}

/// Write `msg` directly to `/dev/tty`, bypassing the test runner's stdout/stderr
/// capture.  Falls back to `eprintln!` when no TTY is available (e.g. CI).
fn tty_println(msg: &str) {
    use std::io::Write;
    if let Ok(mut tty) = std::fs::OpenOptions::new().write(true).open("/dev/tty") {
        let _ = writeln!(tty, "{msg}");
    } else {
        eprintln!("{msg}");
    }
}

/// Print the total detection-accuracy summary accumulated over all iterations.
///
/// Format per category: `{c}ok+{fp}fp+{miss}miss={accuracy:.2}`
fn format_total_summary(
    add: CategoryMetrics,
    del: CategoryMetrics,
    ren: CategoryMetrics,
    mdf: CategoryMetrics,
) -> String {
    let fmt = |label: &str, m: CategoryMetrics| -> String {
        format!(
            "{}({}ok+{}fp+{}miss={:.2})",
            label,
            m.correct,
            m.false_positive,
            m.unrecognized,
            m.accuracy()
        )
    };
    format!(
        "  TOTAL: {} {} {} {}",
        fmt("add", add),
        fmt("del", del),
        fmt("ren", ren),
        fmt("mod", mdf),
    )
}

// ── main iteration ────────────────────────────────────────────────────────────

/// Run one full iteration: cfg.n_chains families, interleaved compression,
/// manifest verification, and roundtrip decompression check.
///
/// `workers` controls the rayon thread-pool size passed to every
/// `compress` / `decompress` call.  Use `1` for a fully sequential baseline
/// and a larger value for the parallel stress variant.
///
/// Returns the per-chain manifest results for external accumulation.
async fn run_iteration(
    iter: usize,
    cfg: StressConfig,
    workers: usize,
) -> Vec<Vec<ManifestCheckResult>> {
    let (storage, compressor) = make_compressor();

    // ── Phase A: build all chains ─────────────────────────────────────────────
    let mut chains: Vec<ChainState> = Vec::with_capacity(cfg.n_chains);
    for chain in 0..cfg.n_chains {
        chains.push(build_chain(iter, chain, cfg, workers, &compressor).await);
    }

    // ── Phase B: interleaved random compression ───────────────────────────────
    // Each chain has cfg.chain_length pending generations.  We randomly pick a
    // chain that still has work, ensuring within-chain ordering.
    let mut rng = StdRng::seed_from_u64(
        cfg.base_seed ^ 0x_feed_c0de_u64 ^ ((iter as u64).wrapping_mul(0x_517c_c1b7)),
    );
    let mut queues: Vec<VecDeque<usize>> = (0..cfg.n_chains)
        .map(|_| (0..cfg.chain_length).collect())
        .collect();

    // manifest_results[chain][gen] collected in compressed order.
    let mut manifest_results: Vec<Vec<ManifestCheckResult>> =
        (0..cfg.n_chains).map(|_| Vec::new()).collect();

    while queues.iter().any(|q| !q.is_empty()) {
        let available: Vec<usize> = (0..cfg.n_chains)
            .filter(|&c| !queues[c].is_empty())
            .collect();
        let c = available[rng.gen_range(0..available.len())];
        let gen = queues[c].pop_front().unwrap();

        let prev_id = if gen == 0 {
            chains[c].base_id.clone()
        } else {
            chains[c].gen_ids[gen - 1].clone()
        };
        let prev_dir = if gen == 0 {
            chains[c].base_dir.path()
        } else {
            chains[c].gen_dirs[gen - 1].path()
        };
        let image_id = chains[c].gen_ids[gen].clone();

        save_root_meta_for_storage(storage.as_ref(), &prev_id).await;
        compressor
            .compress(
                prev_dir,
                chains[c].gen_dirs[gen].path(),
                compress_opts_workers(&image_id, Some(&prev_id), workers),
            )
            .await
            .unwrap();

        // Verify manifest immediately after compression.
        let manifest_bytes = storage.get_manifest(&image_id).unwrap();
        let manifest = Manifest::from_bytes(&manifest_bytes).unwrap();
        let records = fs_records(&manifest);
        let result = verify_manifest_records(
            records,
            prev_dir,
            chains[c].gen_dirs[gen].path(),
            &chains[c].mut_logs[gen],
        );

        manifest_results[c].push(result);
    }

    // ── Phase C: decompress and roundtrip check ───────────────────────────────
    #[allow(clippy::needless_range_loop)]
    for c in 0..cfg.n_chains {
        for gen in 0..cfg.chain_length {
            let image_id = &chains[c].gen_ids[gen];
            let base_root = if gen == 0 {
                chains[c].base_dir.path().to_path_buf()
            } else {
                chains[c].gen_dirs[gen - 1].path().to_path_buf()
            };

            let out_dir = tempfile::tempdir().unwrap();
            compressor
                .decompress(
                    out_dir.path(),
                    decompress_opts_workers(image_id, &base_root, workers),
                )
                .await
                .unwrap();

            let diffs = compare_dirs(chains[c].gen_dirs[gen].path(), out_dir.path());
            assert!(
                diffs.is_empty(),
                "iter={iter} chain={c} gen={gen}: round-trip produced {} diff(s): {diffs:?}",
                diffs.len()
            );
        }
    }

    manifest_results
}

// ── sequential stress test ────────────────────────────────────────────────────

#[tokio::test]
async fn stress_chain_sequential() {
    let cfg = StressConfig::from_env();
    tty_println(&format!(
        "stress_chain: n_iterations={} n_chains={} chain_length={} base_seed={} (replay: STRESS_SEED={})",
        cfg.n_iterations, cfg.n_chains, cfg.chain_length, cfg.base_seed, cfg.base_seed
    ));

    let pb = ProgressBar::new(cfg.n_iterations as u64);
    pb.set_style(
        ProgressStyle::with_template(
            "{spinner:.green} [{elapsed_precise}] [{bar:50.cyan/blue}] {pos}/{len} ({eta})",
        )
        .unwrap()
        .progress_chars("##-"),
    );

    let mut add = CategoryMetrics::default();
    let mut del = CategoryMetrics::default();
    let mut ren = CategoryMetrics::default();
    let mut mdf = CategoryMetrics::default();

    for iter in 0..cfg.n_iterations {
        let results = run_iteration(iter, cfg, 1).await;
        for chain in &results {
            for res in chain {
                add.correct += res.additions.correct;
                add.false_positive += res.additions.false_positive;
                add.unrecognized += res.additions.unrecognized;

                del.correct += res.deletions.correct;
                del.false_positive += res.deletions.false_positive;
                del.unrecognized += res.deletions.unrecognized;

                ren.correct += res.renames.correct;
                ren.false_positive += res.renames.false_positive;
                ren.unrecognized += res.renames.unrecognized;

                mdf.correct += res.modifications.correct;
                mdf.false_positive += res.modifications.false_positive;
                mdf.unrecognized += res.modifications.unrecognized;
            }
        }
        pb.inc(1);
    }

    pb.finish_with_message("all iterations passed");
    tty_println(&format_total_summary(add, del, ren, mdf));
}

// ── parallel stress test ──────────────────────────────────────────────────────

/// Same as [`stress_chain_sequential`] but runs all iterations concurrently
/// on the tokio thread pool.  Each iteration owns its own independent storage
/// and compressor, so there is no shared mutable state between tasks.
#[tokio::test(flavor = "multi_thread")]
async fn stress_chain_parallel() {
    let cfg = StressConfig::from_env();
    tty_println(&format!(
        "stress_chain_parallel: n_iterations={} n_chains={} chain_length={} base_seed={} (replay: STRESS_SEED={})",
        cfg.n_iterations, cfg.n_chains, cfg.chain_length, cfg.base_seed, cfg.base_seed
    ));

    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc as StdArc,
    };
    use tokio::task::JoinSet;

    let pb = ProgressBar::new(cfg.n_iterations as u64);
    pb.set_style(
        ProgressStyle::with_template(
            "{spinner:.green} [{elapsed_precise}] [{bar:50.cyan/blue}] {pos}/{len} ({eta})",
        )
        .unwrap()
        .progress_chars("##-"),
    );

    let pb = std::sync::Arc::new(pb);

    // Atomics for accumulating per-category metrics across all tasks.
    let add_correct = StdArc::new(AtomicUsize::new(0));
    let add_fp = StdArc::new(AtomicUsize::new(0));
    let add_miss = StdArc::new(AtomicUsize::new(0));
    let del_correct = StdArc::new(AtomicUsize::new(0));
    let del_fp = StdArc::new(AtomicUsize::new(0));
    let del_miss = StdArc::new(AtomicUsize::new(0));
    let ren_correct = StdArc::new(AtomicUsize::new(0));
    let ren_fp = StdArc::new(AtomicUsize::new(0));
    let ren_miss = StdArc::new(AtomicUsize::new(0));
    let mdf_correct = StdArc::new(AtomicUsize::new(0));
    let mdf_fp = StdArc::new(AtomicUsize::new(0));
    let mdf_miss = StdArc::new(AtomicUsize::new(0));

    let mut js: JoinSet<()> = JoinSet::new();

    for iter in 0..cfg.n_iterations {
        let pb = std::sync::Arc::clone(&pb);
        let ac = StdArc::clone(&add_correct);
        let af = StdArc::clone(&add_fp);
        let am = StdArc::clone(&add_miss);
        let dc = StdArc::clone(&del_correct);
        let df = StdArc::clone(&del_fp);
        let dm = StdArc::clone(&del_miss);
        let rc = StdArc::clone(&ren_correct);
        let rf = StdArc::clone(&ren_fp);
        let rm = StdArc::clone(&ren_miss);
        let mc = StdArc::clone(&mdf_correct);
        let mf = StdArc::clone(&mdf_fp);
        let mm = StdArc::clone(&mdf_miss);

        js.spawn(async move {
            let results = run_iteration(iter, cfg, N_PAR_WORKERS).await;
            for chain in &results {
                for res in chain {
                    ac.fetch_add(res.additions.correct, Ordering::Relaxed);
                    af.fetch_add(res.additions.false_positive, Ordering::Relaxed);
                    am.fetch_add(res.additions.unrecognized, Ordering::Relaxed);
                    dc.fetch_add(res.deletions.correct, Ordering::Relaxed);
                    df.fetch_add(res.deletions.false_positive, Ordering::Relaxed);
                    dm.fetch_add(res.deletions.unrecognized, Ordering::Relaxed);
                    rc.fetch_add(res.renames.correct, Ordering::Relaxed);
                    rf.fetch_add(res.renames.false_positive, Ordering::Relaxed);
                    rm.fetch_add(res.renames.unrecognized, Ordering::Relaxed);
                    mc.fetch_add(res.modifications.correct, Ordering::Relaxed);
                    mf.fetch_add(res.modifications.false_positive, Ordering::Relaxed);
                    mm.fetch_add(res.modifications.unrecognized, Ordering::Relaxed);
                }
            }
            pb.inc(1);
        });
    }

    // Wait for all iterations — any panic inside a task becomes a JoinError.
    while let Some(res) = js.join_next().await {
        res.expect("parallel stress iteration panicked");
    }

    pb.finish_with_message("all parallel iterations passed");

    let add = CategoryMetrics {
        correct: add_correct.load(Ordering::Relaxed),
        false_positive: add_fp.load(Ordering::Relaxed),
        unrecognized: add_miss.load(Ordering::Relaxed),
    };
    let del = CategoryMetrics {
        correct: del_correct.load(Ordering::Relaxed),
        false_positive: del_fp.load(Ordering::Relaxed),
        unrecognized: del_miss.load(Ordering::Relaxed),
    };
    let ren = CategoryMetrics {
        correct: ren_correct.load(Ordering::Relaxed),
        false_positive: ren_fp.load(Ordering::Relaxed),
        unrecognized: ren_miss.load(Ordering::Relaxed),
    };
    let mdf = CategoryMetrics {
        correct: mdf_correct.load(Ordering::Relaxed),
        false_positive: mdf_fp.load(Ordering::Relaxed),
        unrecognized: mdf_miss.load(Ordering::Relaxed),
    };
    tty_println(&format_total_summary(add, del, ren, mdf));
}
