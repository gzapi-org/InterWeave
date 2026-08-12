#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/test_scan_semantic_collisions.sh
#
# Self-test for scan_semantic_collisions.sh. Each case builds a fake
# architecture/adr/ tree under $TMPDIR and points the scanner at it with
# --root, so no assertion depends on the real repository's contents.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCAN="$SCRIPT_DIR/scan_semantic_collisions.sh"

pass=0
fail=0
ok()  { printf '  ✓ %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf '  ✗ %s\n' "$1" >&2; fail=$((fail + 1)); }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# make_tree <dir> — a valid ADR tree with two distinctly numbered ADRs.
make_tree() {
    mkdir -p "$1/architecture/adr"
    printf '# First\n\n**Status:** Accepted.\n' > "$1/architecture/adr/0001-first.md"
    printf '# Second\n\n**Status:** Accepted.\n' > "$1/architecture/adr/0002-second.md"
}

# Output is captured, never piped into `grep -q`: with pipefail set, a
# `grep -q` that exits early kills the producer with EPIPE and the
# pipeline reports failure on output that was correct.
run()      { bash "$SCAN" --root "$1" 2>&1; }
run_code() { bash "$SCAN" --root "$1" >/dev/null 2>&1; printf '%s' "$?"; }

printf 'test_scan_semantic_collisions\n'

# ── a clean tree passes ──────────────────────────────────────────────────
R="$TMP/clean"; make_tree "$R"
[ "$(run_code "$R")" = "0" ] && ok "clean tree exits 0" || bad "clean tree should exit 0"

# ── two files claiming the same ADR number ───────────────────────────────
R="$TMP/dupnum"; make_tree "$R"
printf '# Also second\n' > "$R/architecture/adr/0002-also-second.md"
[ "$(run_code "$R")" = "1" ] && ok "duplicate ADR number exits 1" || bad "duplicate number should exit 1"
out="$(run "$R")"
[[ "$out" == *"0002"* ]] && ok "  and names the colliding number" || bad "should name 0002"
[[ "$out" == *"0002-second.md"* && "$out" == *"0002-also-second.md"* ]] \
    && ok "  and lists both files" || bad "should list both colliding files"

# ── identical amendment headings inside one ADR ──────────────────────────
R="$TMP/dupamend"; make_tree "$R"
printf '# Third\n\n## Android amendment\n\ntext\n\n## Android amendment\n\nmore\n' \
    > "$R/architecture/adr/0003-third.md"
[ "$(run_code "$R")" = "1" ] && ok "identical amendment headings exit 1" || bad "duplicate headings should exit 1"
out="$(run "$R")"
[[ "$out" == *"0003-third.md"* ]] && ok "  and names the file" || bad "should name 0003-third.md"

# ── DISTINCT amendment headings in one file are fine ─────────────────────
R="$TMP/okamend"; make_tree "$R"
printf '# Third\n\n## Android amendment\n\ntext\n\n## Desktop amendment\n\nmore\n' \
    > "$R/architecture/adr/0003-third.md"
[ "$(run_code "$R")" = "0" ] && ok "distinct amendment headings pass" || bad "distinct headings should pass"

# ── an amendment heading repeated across DIFFERENT files is fine ─────────
R="$TMP/crossfile"; make_tree "$R"
printf '# A\n\n## Android amendment\n' > "$R/architecture/adr/0003-a.md"
printf '# B\n\n## Android amendment\n' > "$R/architecture/adr/0004-b.md"
[ "$(run_code "$R")" = "0" ] && ok "same heading in different ADRs is not a collision" \
    || bad "cross-file heading reuse should pass"

# ── non-ADR files in the directory are ignored ───────────────────────────
R="$TMP/readme"; make_tree "$R"
printf '# Index\n\n## Amendment\n\n## Amendment\n' > "$R/architecture/adr/README.md"
[ "$(run_code "$R")" = "0" ] && ok "README.md is not scanned as an ADR" || bad "README should be ignored"

# ── a missing ADR directory is an invocation problem, not a collision ────
[ "$(run_code "$TMP/nothing-here")" = "2" ] && ok "missing adr/ exits 2" || bad "missing adr/ should exit 2"

# ── the real repository is clean ─────────────────────────────────────────
REAL="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel 2>/dev/null)"
if [ -n "$REAL" ]; then
    [ "$(run_code "$REAL")" = "0" ] && ok "the real repository has no collisions" \
        || bad "the real repository reports collisions"
fi

# ── help and usage ───────────────────────────────────────────────────────
help_out="$(bash "$SCAN" --help 2>/dev/null)"
[[ "$help_out" == *"SEMANTIC collisions"* ]] && ok "--help prints the help block" || bad "--help should print help"
bash "$SCAN" --root >/dev/null 2>&1
[ "$?" = "2" ] && ok "--root without a value exits 2" || bad "--root with no value should exit 2"

printf '\n'
if [ "$fail" -gt 0 ]; then
    printf 'test_scan_semantic_collisions: %d passed, %d FAILED.\n' "$pass" "$fail" >&2
    exit 1
fi
printf 'test_scan_semantic_collisions: OK — all %d assertions passed.\n' "$pass"
