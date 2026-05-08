#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 JulesIMF
#
# image-delta — incremental disk-image compression toolkit
# roundtrip-test.sh — verify compress → decompress roundtrip test

# Usage:
#   roundtrip-test.sh [OPTIONS] <workdir> <base-image> <target-image> <output-name>
#
# Arguments:
#   workdir        Directory that will contain the .imgdelta store (e.g. /tmp)
#   base-image     Absolute path to the base qcow2 image
#   target-image   Absolute path to the target qcow2 image
#   output-name    Filename for the decompressed output image (inside workdir)
#
# Options:
#   -w, --workers N      Number of worker threads (default: nproc)
#   -n, --runs N         Number of times to repeat the roundtrip (default: 3)
#   --no-clean           Do NOT wipe .imgdelta before each run (default: wipe)
#   -h, --help           Show this help and exit
#
# Environment:
#   CI=true              Disable ANSI color output (set automatically by most CI systems)
#   IMGDELTA             Path to the imgdelta binary (default: auto-detect)

set -euo pipefail

# ── colour helpers ────────────────────────────────────────────────────────────
if [[ -t 1 && "${CI:-}" != "true" ]]; then
    RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'
    CYAN='\033[0;36m'; BOLD='\033[1m'; RESET='\033[0m'
else
    RED=''; GREEN=''; YELLOW=''; CYAN=''; BOLD=''; RESET=''
fi

info()    { printf "${CYAN}[INFO]${RESET}  %s\n" "$*"; }
ok()      { printf "${GREEN}[OK]${RESET}    %s\n" "$*"; }
warn()    { printf "${YELLOW}[WARN]${RESET}  %s\n" "$*"; }
fail()    { printf "${RED}[FAIL]${RESET}  %s\n" "$*" >&2; }
header()  { printf "\n${BOLD}=== %s ===${RESET}\n" "$*"; }
step()    { printf "${BOLD}--- %s ---${RESET}\n" "$*"; }

# ── defaults ──────────────────────────────────────────────────────────────────
WORKERS=$(nproc 2>/dev/null || sysctl -n hw.logicalcpu 2>/dev/null || echo 4)
RUNS=3
CLEAN=true

# ── argument parsing ──────────────────────────────────────────────────────────
POSITIONAL=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        -w|--workers)   WORKERS="$2"; shift 2 ;;
        -n|--runs)      RUNS="$2";    shift 2 ;;
        --no-clean)     CLEAN=false;  shift   ;;
        -h|--help)
            sed -n '/^# Usage:/,/^[^#]/{ s/^# \{0,1\}//; p }' "$0" | head -25
            exit 0
            ;;
        -*)  fail "Unknown option: $1"; exit 1 ;;
        *)   POSITIONAL+=("$1"); shift ;;
    esac
done

if [[ ${#POSITIONAL[@]} -ne 4 ]]; then
    fail "Expected 4 positional arguments: workdir base-image target-image output-name"
    echo "Run with --help for usage." >&2
    exit 1
fi

WORKDIR="${POSITIONAL[0]}"
BASE_IMAGE="${POSITIONAL[1]}"
TARGET_IMAGE="${POSITIONAL[2]}"
OUTPUT_NAME="${POSITIONAL[3]}"

# ── locate imgdelta binary ────────────────────────────────────────────────────
if [[ -n "${IMGDELTA:-}" ]]; then
    IMGDELTA_BIN="$IMGDELTA"
elif command -v imgdelta &>/dev/null; then
    IMGDELTA_BIN="imgdelta"
else
    # Look for a release build next to this script's workspace root
    SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    CANDIDATE="$SCRIPT_DIR/../target/release/imgdelta"
    if [[ -x "$CANDIDATE" ]]; then
        IMGDELTA_BIN="$(realpath "$CANDIDATE")"
    else
        fail "imgdelta binary not found. Build with 'cargo build --release' or set IMGDELTA env var."
        exit 1
    fi
fi

# ── validate inputs ───────────────────────────────────────────────────────────
for path in "$BASE_IMAGE" "$TARGET_IMAGE"; do
    if [[ ! -f "$path" ]]; then
        fail "Image not found: $path"
        exit 1
    fi
done

if [[ ! -d "$WORKDIR" ]]; then
    fail "workdir does not exist: $WORKDIR"
    exit 1
fi

# Derive stable image IDs from filenames (strip extension)
BASE_ID="$(basename "$BASE_IMAGE" .qcow2)"
TARGET_ID="$(basename "$TARGET_IMAGE" .qcow2)"
STORE="$WORKDIR/.imgdelta"
OUTPUT="$WORKDIR/$OUTPUT_NAME"

# ── summary ───────────────────────────────────────────────────────────────────
header "imgdelta roundtrip-test"
info "binary    : $IMGDELTA_BIN"
info "workdir   : $WORKDIR"
info "base      : $BASE_IMAGE  (id=$BASE_ID)"
info "target    : $TARGET_IMAGE  (id=$TARGET_ID)"
info "output    : $OUTPUT"
info "workers   : $WORKERS"
info "runs      : $RUNS"
info "clean     : $CLEAN"

FAILED=0
TIMINGS=()

for (( run=1; run<=RUNS; run++ )); do
    header "Run $run / $RUNS"

    # ── optional cleanup ──────────────────────────────────────────────────────
    if [[ "$CLEAN" == "true" && -d "$STORE" ]]; then
        step "Removing $STORE"
        sudo rm -rf "$STORE"
    fi
    [[ -f "$OUTPUT" ]] && sudo rm -f "$OUTPUT"

    RUN_START=$SECONDS

    # ── compress ──────────────────────────────────────────────────────────────
    step "Compress  $BASE_ID → $TARGET_ID"
    if ! (cd "$WORKDIR" && sudo "$IMGDELTA_BIN" compress \
            --base-image    "$BASE_IMAGE" \
            --image         "$TARGET_IMAGE" \
            --base-image-id "$BASE_ID" \
            --image-id      "$TARGET_ID" \
            --workers       "$WORKERS"); then
        fail "compress failed on run $run"
        FAILED=$(( FAILED + 1 ))
        continue
    fi
    ok "compress done"

    # ── decompress ────────────────────────────────────────────────────────────
    step "Decompress $TARGET_ID → $OUTPUT"
    if ! (cd "$WORKDIR" && sudo "$IMGDELTA_BIN" decompress \
            --image-id    "$TARGET_ID" \
            --output      "$OUTPUT" \
            --base-image  "$BASE_IMAGE" \
            --workers     "$WORKERS"); then
        fail "decompress failed on run $run"
        FAILED=$(( FAILED + 1 ))
        continue
    fi
    ok "decompress done"

    # ── output file sanity ────────────────────────────────────────────────────
    if [[ ! -f "$OUTPUT" ]]; then
        fail "output file missing after decompress: $OUTPUT"
        FAILED=$(( FAILED + 1 ))
        continue
    fi

    OUTPUT_SIZE=$(stat -c%s "$OUTPUT" 2>/dev/null || stat -f%z "$OUTPUT")
    TARGET_SIZE=$(stat -c%s "$TARGET_IMAGE" 2>/dev/null || stat -f%z "$TARGET_IMAGE")
    ok "output size : $OUTPUT_SIZE bytes  (target image: $TARGET_SIZE bytes)"

    RUN_ELAPSED=$(( SECONDS - RUN_START ))
    TIMINGS+=("run $run: ${RUN_ELAPSED}s")
    ok "run $run completed in ${RUN_ELAPSED}s"
done

# ── final summary ─────────────────────────────────────────────────────────────
header "Summary"
for t in "${TIMINGS[@]}"; do
    info "$t"
done

PASSED=$(( RUNS - FAILED ))
if [[ $FAILED -eq 0 ]]; then
    ok "All $RUNS runs passed."
    exit 0
else
    fail "$FAILED / $RUNS runs FAILED (passed: $PASSED)"
    exit 1
fi
