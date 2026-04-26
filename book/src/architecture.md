# Architecture

## Overview

imgdelta operates at the **filesystem level**, not at the block device level.
Instead of computing a binary diff of a raw `.qcow2` file, it walks both
image filesystems, detects per-file changes, and encodes each file
individually using the most appropriate algorithm.

```
┌─────────────────────────────────────────────────────────────────────┐
│                         compress()                                  │
│                                                                     │
│  base FS ──┐                                                        │
│            ├──▶ diff_dirs() ──▶ path_match() ──▶ RouterEncoder      │
│  new FS  ──┘         │               │               │             │
│                       │               │               ▼             │
│                    DiffResult     rename         encode() ──▶ blob  │
│                    (4 kinds)      pairs          or patch           │
│                                                       │             │
│                                                       ▼             │
│                                                   Storage           │
│                                              (S3 + PostgreSQL)      │
└─────────────────────────────────────────────────────────────────────┘
```

## Crate split

| Crate              | Type    | Contains                                         |
| ------------------ | ------- | ------------------------------------------------ |
| `image-delta-core` | library | All algorithms, traits, data structures          |
| `image-delta-cli`  | binary  | CLI, S3/PostgreSQL `Storage`, TOML config wiring |

The library has zero dependencies on AWS SDK, `sqlx`, or any other I/O
framework. A provider can embed `image-delta-core` directly and supply their
own `Storage` implementation.

## Module map — `image-delta-core`

```
image-delta-core/src/
├── lib.rs              pub use; crate-level doc
├── encoder.rs          trait DeltaEncoder { encode, decode, algorithm_id }
├── storage.rs          trait Storage (upload/download blobs, patches, manifests)
├── format.rs           trait ImageFormat + MountHandle
├── compressor.rs       trait Compressor + struct DefaultCompressor
├── routing.rs          RouterEncoder, GlobRule, ElfRule, SizeRule, MagicRule
├── manifest.rs         Manifest, Entry, BlobRef, PatchRef, Metadata (MessagePack)
├── error.rs            Error enum, Result alias
├── scheduler.rs        WorkQueue<T> — thread-safe FIFO (wired up in Phase 5)
├── encoders/
│   ├── vcdiff/         Xdelta3Encoder — only file with unsafe (FFI to xdelta3.c)
│   ├── passthrough/    PassthroughEncoder — stores file verbatim
│   └── text_diff/      TextDiffEncoder — Myers line diff (pure Rust)
├── formats/
│   ├── directory.rs    DirectoryFormat — mount = return the dir as-is
│   └── qcow2.rs        Qcow2Format — qemu-nbd + mount (feature = "qcow2")
├── fs_diff/            diff_dirs() → DiffResult, TreeStats
└── path_match/         find_best_matches() — bijective rename detection
```

## Data flow — compression

1. **`diff_dirs(base, target)`** — walks both trees with `walkdir`
   (symlinks not followed). Compares: SHA-256 (skipped when mtime+size match),
   mode, uid, gid, mtime. Returns `DiffResult { diffs, base: TreeStats, target: TreeStats }`.

2. **`find_best_matches(removed, added)`** — scores path similarity with a
   weighted edit distance. Digits get lower substitution cost (version bumps
   are cheap). Returns a bijective set of `PathMatch { source_path, target_path, score }`.

3. **`RouterEncoder::select(file_info)`** — evaluates `[[routing]]` rules in
   order. First rule whose predicate matches wins. Fallback = `default_encoder`.

4. **`DeltaEncoder::encode(src, target)`** — produces a binary delta.
   If `delta.len() >= source.len() * passthrough_threshold`, the delta is
   discarded and the file is stored verbatim (`PassthroughEncoder`).

5. **`Storage::upload_blob(data)`** — stores bytes by content UUID in S3.
   `Storage::upload_manifest(image_id, bytes)` stores the MessagePack manifest.

## Data flow — decompression

1. Download manifest → deserialise.
2. Download patches tar from S3 → extract to temp directory.
3. For each `Entry` in dependency order:
   - `Added` → copy blob to output path
   - `BlobPatch` → `decode(base_blob, patch)` → write output
   - `Removed` → skip
   - `MetadataOnly` → apply mode/uid/gid/mtime from base, write to output
   - `Hardlink` → `fs::hard_link(target, output_path)`
4. Apply all metadata (mode, uid, gid, mtime) after content phase.

## Manifest format

Manifests are serialised with **MessagePack** (`rmp-serde`, `to_vec_named`).
Each manifest consists of a `ManifestHeader` followed by a `Vec<Entry>`.

```rust
struct ManifestHeader {
    version:            u32,      // MANIFEST_VERSION = 1
    image_id:           String,
    base_image_id:      Option<String>,
    algorithm_id:       String,   // e.g. "xdelta3"
    patches_compressed: bool,
}

struct Entry {
    path:            String,
    entry_type:      EntryType,   // Added | Removed | Changed | MetadataOnly | Hardlink
    blob:            Option<BlobRef>,
    patch:           Option<PatchRef>,
    metadata:        Option<Metadata>,
    hardlink_target: Option<String>,
}
```

`None` fields are skipped during serialisation (`skip_serializing_if`),
keeping the manifest compact. JSON equivalent is available for debugging
via `manifest inspect --format json`.

## Encoding strategy per file type

| File type                               | Default encoder     | Rationale                          |
| --------------------------------------- | ------------------- | ---------------------------------- |
| ELF binaries (magic `7fELF`)            | `xdelta3`           | VCDIFF excels at binary similarity |
| Config/script (`*.conf`, `*.py`, …)     | `text_diff`         | Line-level diff is more compact    |
| Already compressed (`*.gz`, `*.zst`, …) | `passthrough`       | Re-encoding wastes CPU             |
| Everything else                         | `xdelta3` (default) | Safe fallback                      |

If a delta is larger than the original (`ratio ≥ passthrough_threshold`),
the delta is silently replaced by a verbatim blob — the `algorithm_id`
`"passthrough"` is recorded in the manifest so decompression is symmetric.

## Concurrency model

Phase 4 runs single-threaded. Phase 5 will wire `WorkQueue<T>` + N worker
threads (one `Xd3Context` per thread — the xdelta3 context is not `Send`).
The scheduler is an `Arc<Mutex<VecDeque<WorkItem>>>` + condvar; no tokio
or async in the hot path.
