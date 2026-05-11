# Introduction

> **Academic project** — imgdelta is a diploma project designed from the ground
> up with industrial applicability in mind. This documentation is written for
> anyone who finds the project and wants to understand the architecture,
> integrate the library, or extend it to fit their own infrastructure.

**imgdelta** is a file-level delta compression toolkit for cloud OS images.

Instead of storing every image version in full, imgdelta computes per-file
binary patches between filesystem snapshots. Patches and blobs are stored in
an object store of your choice; image metadata lives in any backend that
implements the `Storage` trait. Decompression runs fully in-process —
no daemons, no FUSE, no kernel modules required on the client side.

**ubuntu-22.04**, **debian-11**, and **fedora-37** qcow2 images already
compress and decompress correctly end-to-end.

---

## Problem and motivation

Cloud providers store thousands of OS images: public distribution images,
user snapshots, nightly CI builds. The naive approach — store every version
in full — causes storage costs to grow linearly even when successive versions
differ by only 2–5 % of their files.

imgdelta exploits that redundancy:

1. Walk both filesystem trees, compare SHA-256 hashes, detect added /
   removed / changed / renamed files.
2. For each changed file, select the optimal encoding algorithm (VCDIFF,
   text-diff, verbatim) based on file path, type, and magic bytes.
3. Upload patches + manifest to object storage.
4. On decompress, download manifest and patches, apply them to the base
   image, and reconstruct the target filesystem.

### Mathematical sketch

Let $I_0, I_1, \ldots, I_n$ be successive image versions. Full storage cost:

$$S_{\text{full}} = \sum_{k=0}^{n} |I_k|$$

Delta storage cost (imgdelta):

$$S_{\text{delta}} = |I_0| + \sum_{k=1}^{n} \delta(I_{k-1}, I_k)$$

where $\delta(I_{k-1}, I_k)$ is the total compressed size of **only the
changed files**. For typical Linux distribution images
$\delta \approx 3\text{–}10\%$ of $|I_k|$, yielding a $10\text{–}30\times$
reduction compared to full storage.

A patch for file $f$ is encoded as a VCDIFF:

$$\text{patch}(f) = \text{VCDIFF}(f_{\text{base}},\; f_{\text{target}})$$

and reconstructed as:

$$f_{\text{target}} = \text{apply}(f_{\text{base}},\; \text{patch}(f))$$

---

## Who can use imgdelta

| Scenario                                                       | Benefit                                                |
| -------------------------------------------------------------- | ------------------------------------------------------ |
| **Cloud provider** storing public OS distribution images       | Thousands of versions at the cost of a few full images |
| **Cloud provider** managing per-user VM snapshots              | Savings on frequent restore-point workloads            |
| **CI/CD infrastructure** building images on every commit       | Version history storage stays flat                     |
| **Edge provider** with limited uplink bandwidth                | Ship only deltas; reconstruct on-device                |
| **Embedded OS developer** with frequent firmware builds        | Version firmware with minimal storage overhead         |
| **Corporate IT** maintaining a golden-image tree with branches | Each branch stores only its delta from the base        |

### Advantages

- **Maximum efficiency** — file-level content-addressed deduplication; files
  with identical SHA-256 are never uploaded twice.
- **Modularity** — image format, partition type, encoder, and storage backend
  are fully independent and replaceable without touching algorithm code.
- **No read-time overhead** — decompression happens once before VM boot; the
  running system is unaware of imgdelta.
- **Integrity verification** — every patch stores a SHA-256 digest checked at
  decompress time.
- **Composable encoders** — `RouterEncoder` is itself a `PatchEncoder`, so
  sub-routers and nested routing trees are possible.

### Limitations

- **Deep integration required** — maximum efficiency demands a custom `Storage`
  implementation and familiarity with the partition/image model. There is no
  "plug in and forget" mode.
- **No base image → no delta** — the first image in any chain is always stored
  in full.
- **qcow2 requires `qemu-nbd`** — mounting qcow2 images depends on
  `qemu-utils` and Linux kernel NBD support.
- **File-level, not block-level** — does not benefit workloads with encrypted
  or non-mountable filesystems.

---

## Key properties

- **File-level diff** — operates on mounted filesystems, not raw disk blocks
- **Pluggable encoders** — xdelta3 (VCDIFF), text-diff (Myers), passthrough;
  new encoders implement `PatchEncoder`
- **Composable router** — `RouterEncoder` is a `PatchEncoder`; nested
  sub-routers are supported
- **Parallel** — configurable rayon worker pool for compress and decompress
- **Library + CLI** — `image-delta-core` is a reusable Rust library;
  `imgdelta` CLI builds on top of it
- **Offline decompression** — full reconstruction before VM start; no lazy loading
- **Custom Storage** — implement `Storage` for your own backend; all imgdelta
  algorithms work transparently
- **`compress` and `decompress` are free functions** — not methods on `Image`.
  This is deliberate: compressing an image is a heavy, multi-variable operation
  (image format, partition types, filesystem types, storage backend, routing
  config, worker count, …). A method like `image.compress_from(base, storage)`
  would hide that complexity. The free-function signatures make every parameter
  explicit and visible at the call site.
