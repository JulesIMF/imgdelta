# Introduction

**imgdelta** is a delta compression tool for cloud OS images.

Instead of storing each image version in full, imgdelta computes per-file binary
deltas between filesystem snapshots. Patches are stored in S3; metadata is
indexed in PostgreSQL. Decompression is offline and fully in-process (no
daemon, no FUSE).

## Key properties

- **Per-file deltas** — operates on the filesystem level, not on raw disk blocks
- **Pluggable encoders** — xdelta3 (VCDIFF), text-diff (Myers), passthrough;
  routed per file type via TOML configuration
- **Parallel** — configurable worker threads for both compression and decompression
- **Library + CLI** — `image-delta-core` is a reusable Rust library;
  `imgdelta` is the CLI binary
- **Offline decompression** — full reconstruction before VM creation; no lazy loading

## When to use

imgdelta is designed for cloud providers who store many similar OS image versions
(e.g. nightly Debian builds) in cold/archive object storage and need to reduce
storage costs without sacrificing decompression throughput.
