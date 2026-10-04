#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/test_check_vendored_provenance.sh
#
# Self-test for check_vendored_provenance.py. Builds throwaway repository
# roots and crate tarballs under $TMPDIR and runs the check with
# --crate-dir, so no assertion touches the network or cargo's cache --
# except the one that deliberately checks the real tree.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECK="$SCRIPT_DIR/check_vendored_provenance.py"
REAL="$(cd "$SCRIPT_DIR/../.." && pwd)"

pass=0
fail=0
ok()  { printf '  ✓ %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf '  ✗ %s\n' "$1" >&2; fail=$((fail + 1)); }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# Captured, never piped into `grep -q` -- pipefail plus an early-exiting
# grep turns correct output into a failed pipeline.
CRATES="$TMP/crates"
run()      { python3 "$CHECK" --root "$1" --crate-dir "$CRATES" 2>&1; }
run_code() { python3 "$CHECK" --root "$1" --crate-dir "$CRATES" >/dev/null 2>&1; printf '%s' "$?"; }

# The upstream crate, as a registry tarball: a manifest, a source file
# and cargo's packaging files, which a vendored copy leaves out.
mkdir -p "$TMP/src/demo-1.0.0/src" "$CRATES"
cat > "$TMP/src/demo-1.0.0/Cargo.toml" <<'EOF'
[package]
name = "demo"
version = "1.0.0"
EOF
printf 'pub fn answer() -> u32 {\n    41\n}\n' > "$TMP/src/demo-1.0.0/src/lib.rs"
printf '{}\n' > "$TMP/src/demo-1.0.0/.cargo_vcs_info.json"
cp "$TMP/src/demo-1.0.0/Cargo.toml" "$TMP/src/demo-1.0.0/Cargo.toml.orig"
tar czf "$CRATES/demo-1.0.0.crate" -C "$TMP/src" demo-1.0.0
SUM="$(sha256sum "$CRATES/demo-1.0.0.crate" | cut -d' ' -f1)"

# A repository root vendoring it verbatim, its checksum recorded as
# license_exempt.txt records one.
make_root() {
    local r="$1" sum="$2"
    mkdir -p "$r/third_party/demo/src" "$r/tools/checks"
    cp "$TMP/src/demo-1.0.0/Cargo.toml" "$r/third_party/demo/Cargo.toml"
    cp "$TMP/src/demo-1.0.0/src/lib.rs" "$r/third_party/demo/src/lib.rs"
    cat > "$r/tools/checks/license_exempt.txt" <<EOF
# --- third_party/demo -------------------------------------------------------
# demo 1.0.0. Vendored from the crates.io tarball (sha256
# $sum).
third_party/demo/Cargo.toml
third_party/demo/src/lib.rs
EOF
}

# Record the tree's difference from upstream as the repository does:
# from a workspace-shaped root, upstream/ beside third_party/.
record_patch() {
    local r="$1" w="$TMP/patch-$RANDOM"
    mkdir -p "$w/upstream" "$w/third_party"
    cp -r "$TMP/src/demo-1.0.0" "$w/upstream/"
    cp -r "$r/third_party/demo" "$w/third_party/"
    (cd "$w" && diff -ru -x .cargo_vcs_info.json -x Cargo.toml.orig -x INTERWEAVE.patch \
        upstream/demo-1.0.0 third_party/demo) > "$r/third_party/demo/INTERWEAVE.patch"
}

# ── the verbatim copy, and its control ──────────────────────────────────
make_root "$TMP/clean" "$SUM"
out="$(run "$TMP/clean")"; code=$?
[ "$code" = "0" ] && ok "a verbatim copy passes" || bad "a verbatim copy should pass: $out"
[[ "$out" == *"third_party/demo"* ]] && ok "  and the summary names the tree" || bad "  the summary should name third_party/demo: $out"

# ── an edit the patch records, and one it does not ──────────────────────
make_root "$TMP/patched" "$SUM"
sed -i 's/41/42/' "$TMP/patched/third_party/demo/src/lib.rs"
record_patch "$TMP/patched"
[ "$(run_code "$TMP/patched")" = "0" ] && ok "an edit recorded in INTERWEAVE.patch passes" || bad "a recorded edit should pass: $(run "$TMP/patched")"
printf '// unrecorded\n' >> "$TMP/patched/third_party/demo/src/lib.rs"
out="$(run "$TMP/patched")"; code=$?
[ "$code" = "1" ] && ok "an edit outside the patch's hunks fails" || bad "an unrecorded edit beside a patch should exit 1, got $code: $out"

make_root "$TMP/edited" "$SUM"
printf '// unrecorded\n' >> "$TMP/edited/third_party/demo/src/lib.rs"
out="$(run "$TMP/edited")"; code=$?
[ "$code" = "1" ] && ok "an unrecorded edit with no patch fails" || bad "should exit 1, got $code: $out"
[[ "$out" == *"src/lib.rs differs"* ]] && ok "  and names the file" || bad "  should name src/lib.rs: $out"

make_root "$TMP/stale" "$SUM"
sed -i 's/41/42/' "$TMP/stale/third_party/demo/src/lib.rs"
record_patch "$TMP/stale"
sed -i 's/42/43/' "$TMP/stale/third_party/demo/src/lib.rs"
out="$(run "$TMP/stale")"; code=$?
[ "$code" = "1" ] && ok "a patch that no longer reverse-applies fails" || bad "should exit 1, got $code: $out"
[[ "$out" == *"no longer reverse-applies"* ]] && ok "  and says so" || bad "  should say the patch no longer applies: $out"

# ── the file set ────────────────────────────────────────────────────────
make_root "$TMP/extra" "$SUM"
printf 'x\n' > "$TMP/extra/third_party/demo/src/planted.rs"
out="$(run "$TMP/extra")"; code=$?
[ "$code" = "1" ] && [[ "$out" == *"planted.rs is in the tree and not in the tarball"* ]] \
    && ok "a file the tarball lacks fails, named" || bad "a planted file should fail by name ($code): $out"

make_root "$TMP/missing" "$SUM"
rm "$TMP/missing/third_party/demo/src/lib.rs"
out="$(run "$TMP/missing")"; code=$?
[ "$code" = "1" ] && [[ "$out" == *"src/lib.rs is in the tarball and not in the tree"* ]] \
    && ok "a file removed from the tree fails, named" || bad "a missing file should fail by name ($code): $out"

make_root "$TMP/licence" "$SUM"
printf 'the upstream repository licence\n' > "$TMP/licence/third_party/demo/LICENSE"
[ "$(run_code "$TMP/licence")" = "0" ] && ok "an upstream LICENSE the tarball lacks is allowed" || bad "a LICENSE beside the tarball should pass: $(run "$TMP/licence")"

# ── the checksum ────────────────────────────────────────────────────────
make_root "$TMP/wrongsum" "$(printf '0%.0s' $(seq 64))"
out="$(run "$TMP/wrongsum")"; code=$?
[ "$code" = "1" ] && [[ "$out" == *"the record says"* ]] && ok "a tarball whose sha256 is not the recorded one fails" || bad "a checksum mismatch should exit 1 ($code): $out"

make_root "$TMP/nosum" "$SUM"
sed -i '/sha256/,+1d' "$TMP/nosum/tools/checks/license_exempt.txt"
out="$(run "$TMP/nosum")"; code=$?
[ "$code" = "1" ] && [[ "$out" == *"records no sha256"* ]] && ok "a tree with no recorded checksum fails" || bad "no checksum should exit 1 ($code): $out"

# ── could not run ───────────────────────────────────────────────────────
make_root "$TMP/offline" "$SUM"
code="$(python3 "$CHECK" --root "$TMP/offline" --crate-dir "$TMP/empty-dir" >/dev/null 2>&1; printf '%s' "$?")"
[ "$code" = "2" ] && ok "no obtainable tarball exits 2, not 0 or 1" || bad "an unobtainable tarball should exit 2, got $code"

mkdir -p "$TMP/novendor"
out="$(run "$TMP/novendor")"; code=$?
[ "$code" = "0" ] && [[ "$out" == *"nothing is vendored"* ]] && ok "a tree with no third_party/ says why it passes" || bad "no third_party should pass and say so ($code): $out"

# ── the real tree ───────────────────────────────────────────────────────
# Uses cargo's cache or the network; skipped, loudly, when neither has
# the tarballs, so a sandbox without either does not fail on the guard's
# environment rather than its logic.
code="$(python3 "$CHECK" --root "$REAL" >/dev/null 2>&1; printf '%s' "$?")"
case "$code" in
    0) ok "the real vendored trees are their tarballs plus their patches" ;;
    2) printf '  - the real tree was not checked: no tarball obtainable here\n' ;;
    *) bad "the real vendored trees fail: $(python3 "$CHECK" --root "$REAL" 2>&1)" ;;
esac

help_out="$(python3 "$CHECK" --help 2>/dev/null)"
[[ "$help_out" == *"tarball plus its"* ]] && ok "--help prints the help block" || bad "--help should print help"

printf '\n'
if [ "$fail" -gt 0 ]; then
    printf 'test_check_vendored_provenance: %d passed, %d FAILED.\n' "$pass" "$fail" >&2
    exit 1
fi
printf 'test_check_vendored_provenance: OK — all %d assertions passed.\n' "$pass"
