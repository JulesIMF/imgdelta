# CLI Reference

`imgdelta` is the command-line interface for `image-delta-core`. All commands
require a configuration file (`--config`).

## Global flags

| Flag                  | Default  | Description                                          |
| --------------------- | -------- | ---------------------------------------------------- |
| `--config <PATH>`     | required | Path to TOML configuration file                      |
| `--log-level <LEVEL>` | `info`   | Verbosity: `error`, `warn`, `info`, `debug`, `trace` |

## `imgdelta compress`

Compute a delta from a **base** image to a **target** image and upload it
to storage.

```
imgdelta --config imgdelta.toml compress \
    --image-id        <TARGET_ID>         \
    --base-image-id   <BASE_ID>           \
    --source          <BASE_ROOT_PATH>    \
    --target          <TARGET_ROOT_PATH>  \
    [--image-format   directory|qcow2]    \
    [--workers        N]                  \
    [--overwrite]                         \
    [--debug-dir      <PATH>]
```

| Flag              | Default     | Description                                        |
| ----------------- | ----------- | -------------------------------------------------- |
| `--image-id`      | required    | Unique ID for the new image being produced         |
| `--base-image-id` | required    | ID of the base (reference) image                   |
| `--source`        | required    | Mounted root of the base image                     |
| `--target`        | required    | Mounted root of the target image                   |
| `--image-format`  | `directory` | Image format: `directory` or `qcow2`               |
| `--workers`       | from config | Override `[compressor].workers`                    |
| `--overwrite`     | `false`     | Overwrite an existing image with the same ID       |
| `--debug-dir`     | none        | Dump intermediate pipeline state to this directory |

### Exit codes

| Code | Meaning                                                            |
| ---- | ------------------------------------------------------------------ |
| 0    | Success                                                            |
| 1    | Configuration error (missing file, bad TOML, missing required key) |
| 2    | Storage error (upload failed, permission denied, etc.)             |
| 3    | Image error (mount failed, qemu-nbd not found, etc.)               |

## `imgdelta decompress`

Reconstruct a target image from its stored delta and the base image.

```
imgdelta --config imgdelta.toml decompress \
    --image-id      <TARGET_ID>         \
    --base-root     <BASE_ROOT_PATH>    \
    --output        <OUTPUT_PATH>       \
    [--image-format directory|qcow2]    \
    [--workers      N]
```

| Flag             | Default     | Description                                             |
| ---------------- | ----------- | ------------------------------------------------------- |
| `--image-id`     | required    | ID of the delta image to reconstruct                    |
| `--base-root`    | required    | Mounted root of the base image                          |
| `--output`       | required    | Directory where the reconstructed image will be written |
| `--image-format` | `directory` | Image format                                            |
| `--workers`      | from config | Override `[compressor].workers`                         |

## `imgdelta image status`

Print the status (`Pending`, `Ready`, `Deleted`) of a single image.

```
imgdelta --config imgdelta.toml image status --image-id <ID>
```

## `imgdelta image list`

List all image records known to the storage backend.

```
imgdelta --config imgdelta.toml image list
```

Output format:

```
ID                          BASE               STATUS    CREATED
debian-11-20260502          debian-11-20260401 Ready     2026-05-02T10:11:12Z
debian-11-20260401          <base>             Ready     2026-04-01T08:00:00Z
```

## `imgdelta manifest inspect`

Download and pretty-print the manifest for an image.

```
imgdelta --config imgdelta.toml manifest inspect --image-id <ID>
```

The manifest is decoded from MessagePack and printed as JSON (with
human-readable byte sizes for the `size` fields).

## `imgdelta manifest diff`

Print a human-readable summary of which files were added, removed, modified,
or renamed between two image manifests.

```
imgdelta --config imgdelta.toml manifest diff \
    --base-id   <BASE_ID> \
    --target-id <TARGET_ID>
```

## `imgdelta debug walkdir` _(debug builds only)_

Walk a directory tree and print every path together with its SHA-256, size,
mode, uid, gid, and mtime. Useful for diagnosing why a file is or is not
picked up by the compress pipeline.

```
imgdelta --config imgdelta.toml debug walkdir --root <PATH>
```

This subcommand is compiled out in release builds (`#[cfg(debug_assertions)]`).
