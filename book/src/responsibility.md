# Responsibility Split

imgdelta is designed so that a cloud provider can embed `image-delta-core`
as a library and supply their own storage backend, without modifying any
algorithm code.

## What imgdelta owns

| Concern                                      | Where                                     |
| -------------------------------------------- | ----------------------------------------- |
| Filesystem walk and diff                     | `image-delta-core::fs_diff`               |
| Path similarity / rename detection           | `image-delta-core::path_match`            |
| Binary delta encoding (xdelta3 FFI)          | `image-delta-core::encoders::vcdiff`      |
| Text diff encoding (pure Rust Myers)         | `image-delta-core::encoders::text_diff`   |
| Verbatim blob fallback                       | `image-delta-core::encoders::passthrough` |
| File-type routing                            | `image-delta-core::routing`               |
| Manifest serialisation (MessagePack)         | `image-delta-core::manifest`              |
| Compress/decompress orchestration            | `image-delta-core::compressor`            |
| `DirectoryImage` (plain directory mount)     | `image-delta-core::formats::directory`    |
| `Qcow2Image` (qemu-nbd mount, feature-gated) | `image-delta-core::formats::qcow2`        |
| CLI binary `imgdelta`                        | `image-delta-cli`                         |
| S3 + PostgreSQL `Storage` impl               | `image-delta-cli::impls::s3_storage`      |

## What the provider owns

A provider who embeds `image-delta-core` directly must supply:

1. **`impl Storage`** — plugs into any object store + metadata DB of their choice.
2. **`impl Image`** (optional) — if images use a format other than plain
   directories or qcow2, implement `mount()` and `pack()` accordingly.
3. **Routing config** — TOML rules or a programmatic `Vec<Box<dyn RoutingRule>>`
   passed to `RouterEncoder::new(rules, fallback)`.

Everything else — the diff algorithm, xdelta3 FFI, path matching, manifest
format, decompression order — is handled by the library.

## Trait boundaries

```
             ┌─────────────────────────────────┐
             │       image-delta-core           │
             │                                 │
  Storage ◀──┤ DefaultCompressor               │
  (yours) ───▶  + diff_dirs                    │
             │  + path_match                   │
             │  + RouterEncoder → DeltaEncoder │
             │  + Manifest serde               │
             └─────────────────────────────────┘
             ┌─────────────────────────────────┐
             │       image-delta-cli            │
             │                                 │
             │  S3Storage (impl Storage)        │
             │  CLI argument parsing            │
             │  TOML config → RouterEncoder     │
             └─────────────────────────────────┘
```

## Provider integration example

See the [Provider Integration](integration.md) chapter for a complete Rust
example showing how to instantiate `DefaultCompressor` with a custom
`Storage` implementation.
