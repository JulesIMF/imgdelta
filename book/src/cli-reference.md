# CLI Reference

## Global flags

| Flag                  | Default | Description                                                |
| --------------------- | ------- | ---------------------------------------------------------- |
| `-c, --config <FILE>` | —       | Path to TOML configuration file                            |
| `--log-level <LEVEL>` | `info`  | Override log level (`error`/`warn`/`info`/`debug`/`trace`) |
| `-h, --help`          | —       | Print help                                                 |
| `-V, --version`       | —       | Print version                                              |

---

## `imgdelta compress`

Compress a new image relative to a base image and upload patches to S3.

```sh
imgdelta compress \
  --image         <PATH>         # target filesystem root (or qcow2 path)
  --image-id      <ID>           # unique ID for the new image
  --base-image-id <ID>           # ID of the base image (omit for full backup)
  [--image-format directory|qcow2]   # default: directory
  [--workers      <N>]           # override config workers
  --config        <FILE>
```

**Exit codes**: `0` success, `1` error (message on stderr).

---

## `imgdelta decompress`

Reconstruct an image from stored patches.

```sh
imgdelta decompress \
  --image-id  <ID>      # image to reconstruct
  --base-root <PATH>    # filesystem root of the base image
  --output    <PATH>    # directory to write the reconstructed image
  --config    <FILE>
```

The output directory is created if it does not exist. On completion all
file attributes (mode, uid, gid, mtime, symlinks, hardlinks, xattrs) are
identical to the original target filesystem.

---

## `imgdelta image`

Image management subcommands.

### `imgdelta image status`

```sh
imgdelta image status --image-id <ID> --config <FILE>
```

Prints the lifecycle state of an image (`Pending` / `Compressing` /
`Compressed` / `Failed: <reason>`).

Exit code `1` if the image is not found.

### `imgdelta image list`

```sh
imgdelta image list [--format table|json] --config <FILE>
```

Lists all known images with their `image_id`, `base_image_id`, and status.

---

## `imgdelta manifest`

Manifest inspection subcommands.

### `imgdelta manifest inspect`

```sh
imgdelta manifest inspect \
  --image-id <ID> \
  [--format msgpack|json]   # default: json
  --config <FILE>
```

Downloads and prints the manifest for `image-id`. Use `--format json` for
human-readable output or pipe to `jq`.

### `imgdelta manifest diff`

```sh
imgdelta manifest diff \
  --from-image-id <ID> \
  --to-image-id   <ID> \
  --config <FILE>
```

Prints a summary of differences between two manifests.

---

## `imgdelta debug walkdir` _(debug builds only)_

Available only when built without `--release` (`#[cfg(debug_assertions)]`).
Useful for inspecting real directory pairs without a full compress cycle.

```sh
cargo build
./target/debug/imgdelta debug walkdir <OLD_PATH> <NEW_PATH> [--show-metadata=true|false]
```

| Flag              | Default | Description                    |
| ----------------- | ------- | ------------------------------ |
| `--show-metadata` | `true`  | Show `M` (metadata-only) lines |

Output format:

```
+ path/to/added/file
- path/to/removed/file
~ path/to/changed/file
M path/to/metadata-only/file

─── diff summary ────────────────────────
  +  added:         N
  -  removed:       N
  ~  changed:       N
  M  metadata-only: N
  ─────────────────────────────────────────
     total diffs:   N

─── tree stats ──────────────────────────
  old (base)    N files  X.X MiB  N dirs  N symlinks
  new (target)  N files  X.X MiB  N dirs  N symlinks
```
