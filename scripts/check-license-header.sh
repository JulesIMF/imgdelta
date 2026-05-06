#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 JulesIMF
#
# image-delta — incremental disk-image compression toolkit
# Git pre-commit hook: structurally validate SPDX license header in staged files

set -euo pipefail

# ── Constants ─────────────────────────────────────────────────────────────────

readonly MIN_YEAR=2026
readonly CURRENT_YEAR=$(date +%Y)
# em-dash (U+2014) kept as a literal UTF-8 byte sequence for portability
readonly PROJECT_LINE="image-delta — incremental disk-image compression toolkit"

# ── Helper: comment prefix for a file ────────────────────────────────────────
# Echoes the prefix, or empty string if the file type is not checked.
comment_prefix() {
    local file="$1"
    local basename ext
    basename="${file##*/}"
    ext="${file##*.}"
    if [[ "$basename" == .env* ]]; then echo "#"; return; fi
    case "$ext" in
        rs)             echo "//" ;;
        toml|sh|py|env) echo "#"  ;;
        sql)            echo "--" ;;
        *)              echo ""   ;;
    esac
}

# ── Structural validator ──────────────────────────────────────────────────────
# Prints a human-readable reason on failure and returns 1; returns 0 on success.
validate_header() {
    local file="$1"
    local prefix
    prefix=$(comment_prefix "$file")
    [[ -z "$prefix" ]] && return 0

    local -a lines
    mapfile -t lines < <(head -n 12 "$file" 2>/dev/null)

    # Optional shebang for .sh and .py files
    local offset=0
    local ext="${file##*.}"
    if [[ ( "$ext" == "sh" || "$ext" == "py" ) && "${lines[0]:-}" == "#!"* ]]; then
        offset=1
    fi

    local l1="${lines[$offset]:-}"
    local l2="${lines[$((offset+1))]:-}"
    local l3="${lines[$((offset+2))]:-}"
    local l4="${lines[$((offset+3))]:-}"
    local l5="${lines[$((offset+4))]:-}"
    local l6="${lines[$((offset+5))]:-}"

    # Line 1 — exact SPDX identifier
    local exp_l1="${prefix} SPDX-License-Identifier: MIT OR Apache-2.0"
    if [[ "$l1" != "$exp_l1" ]]; then
        echo "line 1: expected '${exp_l1}'"
        echo "        got      '${l1}'"
        return 1
    fi

    # Line 2 — Copyright with year in [MIN_YEAR, CURRENT_YEAR] + non-empty author
    local copy_prefix="${prefix} Copyright (c) "
    if [[ "$l2" != "${copy_prefix}"* ]]; then
        echo "line 2: must start with '${copy_prefix}YYYY <author>'"
        echo "        got '${l2}'"
        return 1
    fi
    local after="${l2#"${copy_prefix}"}"
    local year="${after:0:4}"
    local author="${after:4}"
    if ! [[ "$year" =~ ^[0-9]{4}$ ]]; then
        echo "line 2: '${year}' is not a 4-digit year"
        return 1
    fi
    if (( year < MIN_YEAR || year > CURRENT_YEAR )); then
        echo "line 2: year ${year} must be between ${MIN_YEAR} and ${CURRENT_YEAR}"
        return 1
    fi
    if [[ -z "${author// /}" ]]; then
        echo "line 2: author must not be empty after the year"
        return 1
    fi

    # Line 3 — blank comment (exactly the prefix, nothing more)
    if [[ "$l3" != "$prefix" ]]; then
        echo "line 3: must be '${prefix}' (blank comment)"
        echo "        got '${l3}'"
        return 1
    fi

    # Line 4 — exact project tagline
    local exp_l4="${prefix} ${PROJECT_LINE}"
    if [[ "$l4" != "$exp_l4" ]]; then
        echo "line 4: expected '${exp_l4}'"
        echo "        got      '${l4}'"
        return 1
    fi

    # Line 5 — non-empty file description comment
    if [[ "$l5" != "${prefix} "* || "$l5" == "${prefix} " ]]; then
        echo "line 5: must be a non-empty comment ('${prefix} <description>')"
        echo "        got '${l5}'"
        return 1
    fi

    # Line 6 — truly blank (no comment prefix, no content)
    if [[ -n "$l6" ]]; then
        echo "line 6: must be an empty line"
        echo "        got '${l6}'"
        return 1
    fi

    return 0
}

# ── Main ──────────────────────────────────────────────────────────────────────

if [[ $# -gt 0 ]]; then
    FILES=("$@")
else
    mapfile -t FILES < <(git diff --cached --name-only --diff-filter=ACM)
fi

FAILED=0
CHECKED=0

for file in "${FILES[@]}"; do
    [[ -f "$file" ]] || continue
    prefix=$(comment_prefix "$file")
    [[ -z "$prefix" ]] && continue

    CHECKED=$((CHECKED + 1))

    if ! reason=$(validate_header "$file" 2>&1); then
        echo "bad license header: ${file}" >&2
        while IFS= read -r line; do
            echo "  ${line}" >&2
        done <<< "$reason"
        FAILED=$((FAILED + 1))
    fi
done

if [[ $FAILED -gt 0 ]]; then
    cat >&2 <<EOF

  Expected 6-line header (shown for .rs; adapt prefix for other types):

    // SPDX-License-Identifier: MIT OR Apache-2.0
    // Copyright (c) 2026 <Author>
    //
    // image-delta — incremental disk-image compression toolkit
    // <non-empty description of this file>
    <blank line>

  Prefix by file type:  .rs → //   .toml/.sh/.py/.env → #   .sql → --
  For .sh and .py the header may be preceded by a shebang line.
  Year must be between ${MIN_YEAR} and ${CURRENT_YEAR} (inclusive).

  Run: python3 scripts/add-license-headers.py  to auto-add missing headers.
EOF
    exit 1
fi

[[ $CHECKED -gt 0 ]] && echo "license-header: ok (${CHECKED} file(s) checked)"
exit 0
