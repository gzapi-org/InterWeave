#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/scan_semantic_collisions.sh
#
# >>> help
# Detect SEMANTIC collisions between parallel contributions that a
# textually-clean git merge cannot flag.
#
# Sessions coordinate only through `origin` (CLAUDE.md §Concurrent
# sessions) and allocate sequence numbers independently, so two branches
# can each mint the same ADR number in DIFFERENT files, or write the
# byte-identical heading into the same file, and merge with zero
# conflicts — the merged tree is broken while git reports success.
#
# Checks, over the merged tree:
#   1. ADR file numbers under architecture/adr/ are unique. Two files
#      claiming `0049-` make every "ADR-0049" cross-reference ambiguous
#      and the supersession chain unreadable.
#   2. No ADR contains two identical amendment headings. Amendments are
#      cited by heading; byte-identical headings from parallel sessions
#      must be disambiguated, or every reference to one of them points
#      at both.
#
# On a hit, the fix is to RENUMBER YOUR OWN (newer) entry past the one
# that reached origin first — never to delete or renumber work that
# already landed.
#
# Run it after folding origin/main into your branch and before pushing;
# that is the moment the collision exists and the moment it is cheapest
# to fix.
#
# Options:
#   --root <dir>   scan this repository instead of the one containing
#                  this script
#   -h, --help     this text
#
# Exit codes:
#   0  no collisions
#   1  one or more collisions found
#   2  invocation problem (expected directories missing)
# <<< help

set -uo pipefail

SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
REPO_ROOT="$( cd -- "$SCRIPT_DIR/../.." && pwd )"

show_help() {
    sed -n '/^# >>> help$/,/^# <<< help$/p' "$0" | sed -e '1d' -e '$d' -e 's/^# \{0,1\}//'
}

while [ $# -gt 0 ]; do
    case "$1" in
        -h|--help) show_help; exit 0 ;;
        --root)    [ $# -ge 2 ] || { echo "--root needs a value" >&2; exit 2; }
                   REPO_ROOT="$2"; shift 2 ;;
        *)         echo "scan_semantic_collisions: unexpected argument: $1" >&2; exit 2 ;;
    esac
done

ADR_DIR="$REPO_ROOT/architecture/adr"

if [[ ! -d "$ADR_DIR" ]]; then
    echo "scan_semantic_collisions: expected path not found: $ADR_DIR" >&2
    exit 2
fi

hits=0

fail() {
    echo "FAIL: $1"
    shift
    local line
    for line in "$@"; do
        echo "   $line"
    done
    echo
    hits=$((hits + 1))
}

# ---------------------------------------------------------------------------
# 1. ADR file numbers unique. Numbering is NNNN-slug.md.
# ---------------------------------------------------------------------------
dups=$(find "$ADR_DIR" -maxdepth 1 -name '[0-9][0-9][0-9][0-9]-*.md' -printf '%f\n' \
    | grep -oE '^[0-9]{4}' | sort | uniq -d || true)
if [[ -n "$dups" ]]; then
    while IFS= read -r prefix; do
        [[ -z "$prefix" ]] && continue
        fail "duplicate ADR number ${prefix} — two files claim it." \
            "$(find "$ADR_DIR" -maxdepth 1 -name "${prefix}-*.md" -printf '%f ')" \
            "Renumber YOUR document to the next free number and re-propagate the index."
    done <<< "$dups"
fi

# ---------------------------------------------------------------------------
# 2. No identical amendment headings within one ADR.
# ---------------------------------------------------------------------------
while IFS= read -r adr_file; do
    [[ -z "$adr_file" ]] && continue
    dup_amendments=$(grep -E '^#{2,4} .*[Aa]mendment' "$adr_file" | sort | uniq -d || true)
    if [[ -n "$dup_amendments" ]]; then
        fail "identical amendment headings in $(basename "$adr_file") — cross-references are ambiguous." \
            "$(echo "$dup_amendments" | head -3 | tr '\n' '|')" \
            "Disambiguate the newer heading; do not rewrite the one that landed first."
    fi
done < <(find "$ADR_DIR" -maxdepth 1 -name '[0-9][0-9][0-9][0-9]-*.md')

if (( hits > 0 )); then
    echo "Semantic collision(s): $hits — a textually-clean merge does NOT clear these."
    echo "These are parallel-session numbering races (CLAUDE.md §Concurrent sessions):"
    echo "renumber the entry that has NOT yet reached origin/main; never rewrite landed work."
    exit 1
fi

echo "scan_semantic_collisions: OK — no numbering collisions in the merged tree."
