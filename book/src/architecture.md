# Architecture

## Overview

imgdelta operates at the **filesystem level**, not the block-device level.
Instead of diffing raw disk images, it mounts both the base and target images,
walks their filesystem trees, detects per-file changes, and encodes each
changed file with the most appropriate algorithm.

```mermaid
flowchart TD
    subgraph compress["compress(image, storage, router, source, target, options)"]
        direction TB
        IB["Image::open(base)"] --> DL["DiskLayout<br/>(partition table)"]
        IT["Image::open(target)"] --> DL
        DL --> ITER["iterate partitions<br/>(parallel, matched by index)"]
        ITER --> FSH["FsHandle<br/>filesystem partition"]
        ITER --> BB["BiosBootHandle<br/>BIOS boot GPT"]
        ITER --> MB["MbrHandle<br/>MBR boot code"]
        ITER --> RW["RawHandle<br/>opaque partition"]
        FSH -->|"8-stage file-level pipeline"| ST["Storage"]
        BB -->|"single blob"| ST
        MB -->|"single blob"| ST
        RW -->|"xdelta3 blob"| ST
        ST --> MN["upload_manifest()"]
    end
```

<!-- TODO: turn this into a proper SVG -->

---

## Crates

| Crate                      | Type | Contains                                                               |
| -------------------------- | ---- | ---------------------------------------------------------------------- |
| `image-delta-core`         | lib  | All algorithms, traits, data structures, `LocalStorage` reference impl |
| `image-delta-cli`          | bin  | CLI binary, TOML config wiring, `LocalStorage` instantiation           |
| `image-delta-synthetic-fs` | lib  | Synthetic filesystem tree generator and mutator (test helper)          |

`image-delta-core` has zero dependency on any cloud SDK, database driver, or
HTTP framework. A provider embeds it directly and supplies their own `Storage`.

---

## Module map — `image-delta-core`

```
image-delta-core/src/
├── lib.rs                   pub re-exports; crate-level docs
├── error.rs                 Error enum, Result alias
├── manifest.rs              Manifest, PartitionManifest, Record, BlobRef,
│                            PatchRef, Patch, Data, Metadata  (MessagePack v4)
├── storage/
│   ├── mod.rs               Storage trait + BlobCandidate / ImageMeta / ImageStatus
│   └── local.rs             LocalStorage — filesystem-backed reference impl
├── image/
│   ├── mod.rs               Image trait + OpenImage trait
│   ├── directory.rs         DirectoryImage — plain directory (no mounting)
│   └── qcow2.rs             Qcow2Image — qemu-nbd mount (feature = "qcow2")
├── partitions/
│   ├── mod.rs               DiskScheme / PartitionKind / PartitionDescriptor / DiskLayout
│   ├── fs.rs                FsHandle + MountHandle + SimpleMountHandle
│   ├── mbr.rs               MbrHandle (MBR boot-code area)
│   ├── bios_boot.rs         BiosBootHandle (BIOS Boot GPT partition)
│   └── raw.rs               RawHandle (unknown / opaque partition)
├── encoding/
│   ├── mod.rs               PatchAlgorithm (internal), PatchEncoder trait,
│   │                        AlgorithmCode (u8), FileSnapshot, FilePatch
│   ├── router.rs            RouterEncoder, RoutingRule, FileInfo, GlobRule,
│   │                        ElfRule, SizeRule, MagicRule
│   ├── xdelta3/             Xdelta3Encoder (FFI to vendored xdelta3.c)
│   ├── text_diff.rs         TextDiffEncoder (Myers line-diff, pure Rust)
│   └── passthrough.rs       PassthroughEncoder (verbatim blob)
├── fs_diff/                 diff_dirs() → DiffResult, FileDiff, TreeStats
├── path_match/              find_best_matches() — bijective rename detection
├── compress/
│   ├── mod.rs               compress_fs_partition() entry point
│   └── partitions/
│       ├── fs/
│       │   ├── pipeline.rs  CompressPipeline — runs stages 2–7 in order
│       │   ├── stage.rs     CompressStage trait
│       │   ├── context.rs   StageContext (storage, router, image_id, …)
│       │   ├── draft.rs     FsDraft — mutable working state across stages
│       │   └── stages/
│       │       ├── walkdir.rs          Stage 1 — filesystem walk + SHA-256
│       │       ├── blob_lookup.rs      Stage 2 — match new files to base blobs
│       │       ├── match_renamed.rs    Stage 3 — rename detection
│       │       ├── cleanup.rs          Stage 4 — remove superseded records
│       │       ├── upload_blobs.rs     Stage 5 — upload verbatim blobs
│       │       ├── download_blobs.rs   Stage 6 — fetch base blobs for patching
│       │       ├── compute_patches.rs  Stage 7 — RouterEncoder per changed file
│       │       └── pack_archive.rs     Stage 8 — pack patches.tar + upload
│       ├── bios_boot.rs    BiosBootCompressor
│       ├── mbr.rs          MbrCompressor
│       └── raw_partition.rs RawPartitionCompressor
├── decompress/              Mirror of compress/ — stages for each record kind
└── operations/
    ├── compress.rs          compress() free function — full orchestrator
    ├── decompress.rs        decompress() free function — full orchestrator
    └── delete.rs            delete_image() — remove blobs + manifest
```

---

## Modularity

Every major concern is behind a narrow trait. The three core traits —
`Image`/`OpenImage`, `Storage`, and `PatchEncoder` — have no dependencies on
each other. A new image format, a new storage backend, or a new encoder can
be added without touching any other trait.

### Image abstraction

`Image` is a factory: given a path it opens the image and returns an `OpenImage`
handle. `OpenImage` is a live, mounted handle that exposes the partition layout
and keeps any OS resources (qemu-nbd connection, loop device, FUSE mount)
alive until it is dropped. Each format provides both implementations.

```mermaid
classDiagram
    class Image {
        <<trait>>
        Opens an image path
        Returns a live OpenImage handle
    }
    class OpenImage {
        <<trait>>
        Live image handle
        Exposes DiskLayout and PartitionHandles
        Keeps OS resources alive until dropped
    }
    class DirectoryImage {
        Plain directory tree
        No mounting required
    }
    class Qcow2Image {
        qemu-nbd mount
        feature = "qcow2"
    }
    class TarRootfsImage {
        TAR rootfs archive
        example custom impl
    }
    class RawImgImage {
        Raw .img disk file
        loop device or nbd
    }
    Image <|.. DirectoryImage : implements
    Image <|.. Qcow2Image : implements
    Image <|.. TarRootfsImage : implements
    Image <|.. RawImgImage : implements
    OpenImage <|.. DirectoryImage : implements
    OpenImage <|.. Qcow2Image : implements
    OpenImage <|.. TarRootfsImage : implements
    OpenImage <|.. RawImgImage : implements
    Image --> OpenImage : open() produces
```

<!-- TODO: turn this into a proper SVG -->

### Storage backend

`Storage` is the sole interface between imgdelta and any persistence layer.
It covers three concerns: a content-addressable blob store (SHA-256 keyed),
image metadata (id, status, base relationship), and blob-origin tracking
for cross-image delta reuse. `LocalStorage` is the built-in reference
implementation; providers replace it with their own.

```mermaid
classDiagram
    class Storage {
        <<trait>>
        Content-addressable blob store
        Image metadata registry
        Patch and manifest persistence
        Blob origin tracking for delta reuse
    }
    class LocalStorage {
        Filesystem-backed reference impl
        UUID v5 deterministic blob IDs
        gzip-compressed blobs on disk
        JSON index files
    }
    class ProviderStorage {
        <<implement this>>
        S3, GCS, custom object store
        Any async backend
    }
    Storage <|.. LocalStorage : built-in
    Storage <|.. ProviderStorage : your impl
```

<!-- TODO: turn this into a proper SVG -->

### Encoding

`PatchEncoder` produces a binary delta from source → target and can reconstruct
target from source + delta. Each implementation tags its output with a one-byte
`AlgorithmCode` so decompression is always symmetric.

`RouterEncoder` is itself a `PatchEncoder` that delegates to the first matching
rule. Because it implements the same trait, routers can be nested: one router
can be the fallback of another, enabling tree-shaped routing policies.

```mermaid
classDiagram
    class PatchEncoder {
        <<trait>>
        Produces a binary delta src → target
        Reconstructs target from src + delta
        Tagged with a 1-byte AlgorithmCode
    }
    class RouterEncoder {
        Selects encoder by file properties
        Itself implements PatchEncoder
        Supports nested sub-routers
    }
    class RoutingRule {
        <<trait>>
        Matches a file by glob / ELF magic / size / MIME
        Returns the encoder to use
    }
    class Xdelta3Encoder {
        VCDIFF format
        FFI to vendored xdelta3.c
        Best for ELF binaries and binary data
    }
    class TextDiffEncoder {
        Myers line-level diff
        Pure Rust - no FFI
        Best for config files and scripts
    }
    class PassthroughEncoder {
        No encoding — verbatim blob
        Used for already-compressed data
        Auto-selected when delta exceeds source
    }
    PatchEncoder <|.. RouterEncoder
    PatchEncoder <|.. Xdelta3Encoder
    PatchEncoder <|.. TextDiffEncoder
    PatchEncoder <|.. PassthroughEncoder
    RouterEncoder o-- RoutingRule
    RoutingRule --> PatchEncoder : returns encoder
```

<!-- TODO: turn this into a proper SVG -->

---

## Filesystem partition pipeline

Non-filesystem partitions (MBR boot code, BIOS boot, raw/opaque) are stored as
a single blob or a single xdelta3 diff — no further structure is assumed.

Filesystem partitions go through an 8-stage sequential pipeline. Stages are
run by `CompressPipeline`; stage 8 is called directly after.

```mermaid
flowchart TD
    subgraph fspipe["FsHandle — compress pipeline"]
        direction TB
        WD["1. walkdir<br/>walk base + target, SHA-256 per file<br/>build FsDraft of changed entries"]
        BL["2. blob_lookup<br/>match new files to existing base blobs<br/>by path similarity via Storage"]
        MR["3. match_renamed<br/>rename detection — weighted edit-distance<br/>path scoring, bijective matching"]
        CL["4. cleanup<br/>remove records superseded<br/>by blob-lookup or rename"]
        UB["5. upload_blobs<br/>upload verbatim blobs for added files<br/>record blob origin in Storage"]
        DB["6. download_blobs<br/>fetch base blobs needed<br/>as delta sources"]
        CP["7. compute_patches<br/>RouterEncoder::encode() per file<br/>produce PatchRef records"]
        PA["8. pack_archive<br/>pack patches.tar.gz<br/>upload via Storage::upload_patches"]
        WD --> BL --> MR --> CL --> UB --> DB --> CP --> PA
    end
```

<!-- TODO: turn this into a proper SVG -->

| Stage | Name              | What it does                                                                                                        |
| ----- | ----------------- | ------------------------------------------------------------------------------------------------------------------- |
| 1     | `walkdir`         | Walk base + target roots; compute SHA-256 per file; build initial `FsDraft` with only changed entries               |
| 2     | `blob_lookup`     | Query `Storage::find_blob_candidates` for the base image; match new files to existing base blobs by path similarity |
| 3     | `match_renamed`   | Detect renames using weighted edit-distance path scoring; upgrade matched records to delta candidates               |
| 4     | `cleanup`         | Remove records superseded by blob-lookup or rename matching                                                         |
| 5     | `upload_blobs`    | Upload remaining verbatim (LazyBlob) files to the blob store; record blob origin                                    |
| 6     | `download_blobs`  | Download base blobs needed as delta sources for changed files                                                       |
| 7     | `compute_patches` | Run `RouterEncoder::encode` per file; produce `PatchRef` records                                                    |
| 8     | `pack_archive`    | Pack all patches into `patches.tar.gz`; upload via `Storage::upload_patches`                                        |

---

## Decompression pipeline

Decompression mirrors compression. For each partition the following stages run:

```mermaid
flowchart LR
    ST["Storage"] -->|download_manifest| MF["Manifest<br/>(MessagePack)"]
    ST -->|download_patches| AR["patches.tar.gz"]
    AR --> EX["extract to tmpdir"]
    MF --> AP["apply per record"]
    EX --> AP
    BM["base mount"] --> AP
    AP --> OUT["output directory<br/>(exact replica)"]
```

<!-- TODO: turn this into a proper SVG -->

For each record in the manifest the decompressor dispatches on record type:

| Stage             | Records handled                                           |
| ----------------- | --------------------------------------------------------- |
| `extract_archive` | Download + extract `patches.tar.gz` to a temp directory   |
| `download_blobs`  | Fetch verbatim blobs for `Added` files                    |
| `copy_unchanged`  | Copy unchanged files from the base mount                  |
| `add_records`     | Write added files (from blobs) to output                  |
| `change_records`  | Apply patches to changed files                            |
| `rename_records`  | Copy renamed files from base and apply patches if any     |
| `delete_records`  | Skip removed files                                        |
| `apply_records`   | Set mode / uid / gid / mtime / xattrs on all output paths |

---

## Manifest format

Manifests are serialised with **MessagePack** (`rmp-serde`, `to_vec_named`).
The current schema version is `4`.

```
Manifest {
    header: ManifestHeader {
        version:            u32          // MANIFEST_VERSION = 4
        image_id:           String
        base_image_id:      Option<String>
        format:             String       // "directory" | "qcow2" | …
        created_at:         u64          // Unix timestamp (seconds)
        patches_compressed: bool         // true → patches.tar.gz
    }
    disk_layout: DiskLayout {
        scheme: DiskScheme               // Gpt | Mbr | SingleFs
        partitions: Vec<PartitionDescriptor>
    }
    partitions: Vec<PartitionManifest> {
        descriptor: PartitionDescriptor
        content: PartitionContent        // Fs { records } | BiosBoot { blob_id }
                                         // | Raw { blob_id } | Mbr { blob_id }
    }
}
```

Each `Record` inside `Fs` content represents **one changed path** in the
filesystem. Unchanged files are absent from the manifest and taken directly
from the base image during decompression.

`None` / empty fields use `#[serde(skip_serializing_if)]` to keep the manifest
compact. A JSON debug view is available via `imgdelta manifest inspect --format json`.

---

## Partition types

Different regions of a disk image have very different internal structure.
imgdelta uses a distinct handler for each, so the representation is always
as compact and semantically correct as possible.

```mermaid
graph LR
    IMG["disk image"] --> MBR["MbrHandle<br/>partition 0 (synthetic)"]
    IMG --> BIOS["BiosBootHandle<br/>BIOS Boot GPT"]
    IMG --> FS["FsHandle<br/>filesystem partition<br/>ext4 / xfs / vfat / …"]
    IMG --> RAW["RawHandle<br/>unknown / opaque"]

    MBR -->|"single blob"| ST["Storage"]
    BIOS -->|"single blob"| ST
    FS -->|"8-stage file-level pipeline"| ST
    RAW -->|"xdelta3 blob"| ST
```

<!-- TODO: turn this into a proper SVG -->

| Type                    | Handler          | What it covers                                                        | Strategy                                      | Why                                                                                                           |
| ----------------------- | ---------------- | --------------------------------------------------------------------- | --------------------------------------------- | ------------------------------------------------------------------------------------------------------------- |
| Filesystem partition    | `FsHandle`       | ext4, xfs, vfat, btrfs, …                                             | 8-stage file-level pipeline                   | Files are the natural unit of change; file-level deltas are orders of magnitude smaller than block-level ones |
| MBR boot code           | `MbrHandle`      | First 446 bytes of sector 0 (boot code area, not the partition table) | Single verbatim blob, SHA-256 dedup           | Tiny and changes rarely; file-level diff is meaningless for raw machine code                                  |
| BIOS Boot GPT partition | `BiosBootHandle` | The GPT BIOS boot partition (typically 1 MiB, no filesystem)          | Single verbatim blob                          | No filesystem structure to exploit; content is opaque GRUB stage-2 code                                       |
| Unknown / opaque        | `RawHandle`      | Any partition with an unrecognised type GUID or no filesystem         | xdelta3 binary diff of the raw partition data | Better than verbatim for large opaque regions that still have binary similarity between versions              |

The `MbrHandle` partition is **synthetic**: qcow2 images store the MBR boot
code in sector 0 outside of any partition table entry, so imgdelta creates
a virtual partition 0 to make the manifest representation uniform.

---

## Encoding strategy

| File type                                       | Default encoder      | Rationale                                |
| ----------------------------------------------- | -------------------- | ---------------------------------------- |
| ELF binaries (magic `\x7fELF`)                  | `xdelta3`            | VCDIFF handles binary similarity well    |
| Config / script files                           | `text_diff`          | Line-level diff is more compact for text |
| Already compressed (`*.gz`, `*.zst`, `*.xz`, …) | `passthrough`        | Re-encoding wastes CPU                   |
| Images / media                                  | `passthrough`        | Incompressible                           |
| Everything else                                 | `xdelta3` (fallback) | Safe default                             |

If a computed delta is larger than the source file
(`delta_size >= source_size * passthrough_threshold`), the patch is discarded
and the file is stored verbatim with `AlgorithmCode::Passthrough`. The
manifest records the actual algorithm so decompression is always symmetric.

`AlgorithmCode` is a one-byte tag stored in each `PatchRef`:

| Code   | Algorithm                                 |
| ------ | ----------------------------------------- |
| `0x00` | `Passthrough`                             |
| `0x01` | `Xdelta3`                                 |
| `0x02` | `TextDiff`                                |
| `0xFF` | `Extended` (algorithm id in string field) |

Codes `0x03–0xFE` are reserved for future built-in algorithms.
