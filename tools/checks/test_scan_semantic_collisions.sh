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

# The hand-over to agent-fabric's tools/fabric/github module for scan-semantic-collisions: every case above ran the
# real one (CI points AGENT_FABRIC_ROOT at its pinned checkout); these pin
# what the hand-over itself promises, against a recording stub.
hcheck() { if eval "$2"; then ok "$1"; else bad "$1" "$hout"; fi; }
hstub="$(mktemp -d)"
# The module stub is a shell script; a fake interpreter runs it with bash.
printf '#!/bin/sh\nexec bash "$@"\n' > "$hstub/fake-python"; chmod +x "$hstub/fake-python"
export AGENT_FABRIC_PYTHON="$hstub/fake-python"
mkdir -p "$hstub/fabric/projects/interweave/integration/gh"
printf '{}' > "$hstub/fabric/projects/interweave/integration/gh/collisions.json"
mkdir -p "$(dirname "$hstub/fabric/tools/fabric/github/semantic_collisions.py")"
cat > "$hstub/fabric/tools/fabric/github/semantic_collisions.py" <<'STUB'
#!/usr/bin/env bash
printf 'config=%s cache=%s' "${AGENT_FABRIC_COLLISIONS_CONFIG-<unset>}" "${AGENT_FABRIC_TOOL_CACHE-<unset>}"; printf ' [%s]' "$@"; echo
exit 3
STUB
# shellcheck disable=SC2034 # read in hcheck's eval'd conditions
hrun() { hout="$(env -u AGENT_FABRIC_COLLISIONS_CONFIG -u AGENT_FABRIC_TOOL_CACHE -u INTERWEAVE_TOOL_CACHE AGENT_FABRIC_ROOT="$hstub/fabric" "$@" 2>&1)"; hrc=$?; }
# shellcheck disable=SC2034 # read in hcheck's eval'd conditions
hrepo="$( cd -- "$SCRIPT_DIR/../.." && pwd )"
hrun bash "$SCAN" --root /elsewhere
hcheck "the fabric's scan-semantic-collisions gets this working copy as --root, the caller's arguments after it" '[[ $hrc -eq 3 && "$hout" == *" [--root] [$hrepo] [--root] [/elsewhere]" ]]'
hrun bash "$SCAN" --help
hcheck "--help is this file's own block, not the fabric's" '[[ $hrc -eq 0 && "$hout" != *"[--help]"* && "$hout" == *"agent-fabric"* ]]'
hrun bash "$SCAN"
hcheck "InterWeave's collisions.json is named" '[[ "$hout" == "config=$hstub/fabric/projects/interweave/integration/gh/collisions.json "* ]]'
hrun env AGENT_FABRIC_COLLISIONS_CONFIG=/x.json bash "$SCAN"
hcheck "an explicit AGENT_FABRIC_COLLISIONS_CONFIG wins" '[[ "$hout" == "config=/x.json "* ]]'
hout="$(AGENT_FABRIC_ROOT="$hstub/none" bash "$SCAN" x 2>&1)"
# shellcheck disable=SC2034 # read in hcheck's eval'd conditions
hrc=$?
hcheck "no agent-fabric: exit 2, naming where it looked" '[[ $hrc -eq 2 && "$hout" == *"agent-fabric not found at $hstub/none"* ]]'
rm -rf "$hstub"

printf '\n'
if [ "$fail" -gt 0 ]; then
    printf 'test_scan_semantic_collisions: %d passed, %d FAILED.\n' "$pass" "$fail" >&2
    exit 1
fi
printf 'test_scan_semantic_collisions: OK — all %d assertions passed.\n' "$pass"
