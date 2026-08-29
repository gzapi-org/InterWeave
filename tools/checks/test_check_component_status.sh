#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# Self-test for check_component_status.sh.
#
# The guard's whole value is that it fails on a specific shape, so each
# case builds that shape rather than asserting on the OK message.

set -uo pipefail

SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
GUARD="$SCRIPT_DIR/check_component_status.sh"
[ -f "$GUARD" ] || { echo "test: guard not found at $GUARD" >&2; exit 1; }

failures=0
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

ok()  { echo "  ✓ $1"; }
bad() { echo "  ✗ $1" >&2; failures=$((failures + 1)); }

PLACEHOLDER='**Current status:** planned crate boundary only; no `Cargo.toml` or Rust source yet.'
ACTIVE='**Current status:** Stage 4, active workspace member.'

# Builds one component directory. $3 chooses the status line, $4 whether
# the directory actually holds a crate.
component() {
    local root="$1" name="$2" status="$3" real="$4"
    mkdir -p "$root/crates/$name"
    printf '# %s\n\n%s\n' "$name" "$status" > "$root/crates/$name/README.md"
    if [ "$real" = "real" ]; then
        mkdir -p "$root/crates/$name/src"
        printf '[package]\nname = "x"\n' > "$root/crates/$name/Cargo.toml"
        printf '// code\n' > "$root/crates/$name/src/lib.rs"
    fi
}

run_code() { bash "$GUARD" --root "$1" >/dev/null 2>&1; printf '%s' "$?"; }
run()      { bash "$GUARD" --root "$1" 2>&1; }

printf 'test_check_component_status\n'

echo "check_component_status: a placeholder README over a placeholder directory passes"
R="$TMP/planned"; mkdir -p "$R"
component "$R" "notyet" "$PLACEHOLDER" "planned"
[ "$(run_code "$R")" = "0" ] && ok "a genuine placeholder is fine" || bad "should pass: $(run "$R")"

echo "check_component_status: an active README over real code passes"
R="$TMP/active"; mkdir -p "$R"
component "$R" "live" "$ACTIVE" "real"
[ "$(run_code "$R")" = "0" ] && ok "an accurate README is fine" || bad "should pass: $(run "$R")"

echo "check_component_status: the drift that actually happened"
# Three crates were activated and their READMEs still said the code did
# not exist — the first thing a reader opens, telling them not to look.
R="$TMP/drift"; mkdir -p "$R"
component "$R" "live" "$PLACEHOLDER" "real"
out="$(run "$R")"
[ "$(run_code "$R")" = "1" ] && ok "a README denying its own code fails" || bad "drift should exit 1"
case "$out" in
    *"crates/live/README.md"*) ok "  and names the file" ;;
    *) bad "should name the offending README: $out" ;;
esac

echo "check_component_status: a manifest with no src/ is not yet a crate"
# A directory mid-activation should not be reported: the README is still
# telling the truth about the source.
R="$TMP/partial"; mkdir -p "$R/crates/half"
printf '# half\n\n%s\n' "$PLACEHOLDER" > "$R/crates/half/README.md"
printf '[package]\nname = "x"\n' > "$R/crates/half/Cargo.toml"
[ "$(run_code "$R")" = "0" ] && ok "a manifest alone does not trigger it" || bad "should pass: $(run "$R")"

echo "check_component_status: a parent directory of crates makes no claim"
R="$TMP/parent"; mkdir -p "$R/crates"
printf '# crates\n\n%s\n' "$PLACEHOLDER" > "$R/crates/README.md"
component "$R" "child" "$ACTIVE" "real"
[ "$(run_code "$R")" = "0" ] && ok "a non-crate directory is not judged" || bad "should pass: $(run "$R")"

echo "check_component_status: a missing tree is an invocation error"
[ "$(run_code "$TMP/nope")" = "2" ] && ok "no such directory" || bad "missing root should exit 2"

echo "check_component_status: --help works and says nothing about failure"
bash "$GUARD" --help >/dev/null 2>&1 && ok "--help prints the help block" || bad "--help should exit 0"

echo "check_component_status: the real repository passes its own guard"
if [ "$(run_code "$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)")" = "0" ]; then
    ok "this repository"
else
    bad "this repository fails: $(run "$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)")"
fi

echo "check_component_status: --root with no value is an invocation error"
# Guarded by `timeout`: the failure mode is an infinite loop, so a bare
# assertion would hang the suite rather than fail it.
if timeout 5 bash "$GUARD" --root >/dev/null 2>&1; then
    bad "--root with no value should be an invocation error"
elif [ "$?" = 124 ]; then
    bad "--root with no value hung instead of failing"
else
    ok "--root with no value"
fi

echo
if [ "$failures" -eq 0 ]; then
    echo "test_check_component_status: OK — all assertions passed."
    exit 0
fi
echo "test_check_component_status: FAILED — $failures assertion(s) failed." >&2
exit 1
