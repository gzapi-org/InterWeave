#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/test_check_yamux_muxer.sh
#
# Self-test for check_yamux_muxer.sh.
#
# The guard exists because `cargo-deny` structurally cannot catch this —
# yamux has no RustSec advisory — so the guard is the only mechanism, and
# a guard that cannot fail is worse than none: it reports OK forever and
# reads as coverage.
#
# The cases that matter are therefore the POSITIVE ones: a real setter
# call must be found. The negative cases exist so it does not fail on
# every unrelated `set_max_num_streams` in the tree and get deleted for
# crying wolf.
#
# Exit codes:
#   0  all assertions passed
#   1  one or more failed

set -uo pipefail

SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
UNDER_TEST="$SCRIPT_DIR/check_yamux_muxer.sh"
[[ -f "$UNDER_TEST" ]] || { echo "test: $UNDER_TEST not found" >&2; exit 1; }

failures=0
GUARD_MARKER="$(mktemp)"
command_not_found_handle() {
    printf '%s\n' "$1" >> "$GUARD_MARKER"
    echo "  ✗ self-test bug: called '$1', which this suite does not define" >&2
    return 127
}

SANDBOX=""
cleanup() { [[ -n "$SANDBOX" && -d "$SANDBOX" ]] && rm -rf "$SANDBOX"; }
trap cleanup EXIT

pass() { echo "  ✓ $1"; }
fail() { echo "  ✗ $1" >&2; printf '%s\n' "${2:-}" | sed 's/^/      /' >&2
         failures=$((failures + 1)); }

# A throwaway repository with one source file, so the guard runs against
# `git ls-files` exactly as it does for real.
run_against() {
    SANDBOX="$(mktemp -d)"
    mkdir -p "$SANDBOX/tools/checks" "$SANDBOX/src"
    cp "$UNDER_TEST" "$SANDBOX/tools/checks/"
    printf '%s\n' "$1" > "$SANDBOX/src/lib.rs"
    git -C "$SANDBOX" init -q
    git -C "$SANDBOX" add -A
    RUN_OUT="$(cd "$SANDBOX" && bash tools/checks/check_yamux_muxer.sh 2>&1)"
    RUN_RC=$?
    rm -rf "$SANDBOX"; SANDBOX=""
}

assert_rc() {
    if [[ "$RUN_RC" -eq "$2" ]]; then pass "$1"
    else fail "$1 — expected exit $2, got $RUN_RC" "$RUN_OUT"; fi
}
assert_contains() {
    if [[ "$RUN_OUT" == *"$2"* ]]; then pass "$1"
    else fail "$1 — output lacked '$2'" "$RUN_OUT"; fi
}

echo "check_yamux_muxer.sh — the setters that downgrade the muxer"

# THE CASE THE GUARD EXISTS FOR. `set_max_num_streams` is the one
# CLAUDE.md §6 makes tempting: it reads as bounding a resource.
run_against 'let mut cfg = yamux::Config::default();
cfg.set_max_num_streams(64);'
assert_rc       "a max-num-streams call is caught" 1
assert_contains "  and the file and line are named" "src/lib.rs:2"
assert_contains "  and the advisory is named"       "GHSA-vxx9-2994-q338"
assert_contains "  and cargo-deny's blind spot is stated" "no RustSec advisory"

for setter in set_receive_window_size set_max_buffer_size set_window_update_mode; do
    run_against "let mut cfg = yamux::Config::default();
cfg.$setter(1);"
    assert_rc "a $setter call is caught too" 1
done

echo "check_yamux_muxer.sh — what it must NOT flag"

# The shape the transport actually uses.
run_against 'let cfg = yamux::Config::default();'
assert_rc "a plain default is fine" 0

# AN IDENTICALLY-NAMED SETTER IN A FILE WITH NO YAMUX. This is what
# keeps the guard from crying wolf across the tree; without it the check
# fails on unrelated code and gets deleted, which is how a guard dies.
run_against 'let mut pool = ConnectionPool::new();
pool.set_max_num_streams(8);'
assert_rc "the same setter in a file that never mentions yamux is not flagged" 0

echo "check_yamux_muxer.sh — distance must not hide the call"

# THE BLIND SPOT THIS REPLACES. The first version required `yamux`
# within two lines of the setter, and a self-test asserted that a
# mention four lines up "does not reach" — testing the limitation as
# though it were a feature. Anything between the declaration and the
# call defeated it, and the call still selects 0.12.1 whatever sits
# above it.
run_against 'let mut config = yamux::Config::default();
// validation, a conditional, or just an explanation
// spanning more than the old two-line window
config.set_max_num_streams(64);'
assert_rc       "a setter four lines below the declaration is caught" 1
assert_contains "  and the line is still named"                       "src/lib.rs:4"

# Distance in the other direction too: the declaration may follow.
run_against 'fn configure(config: &mut yamux::Config) {
    // an argument rather than a local, and the mention is on line 1
    let bound = 64;
    let _ = bound;
    config.set_receive_window_size(bound);
}'
assert_rc "and a setter on a yamux-typed argument is caught" 1

if [[ -s "$GUARD_MARKER" ]]; then
    echo "  ✗ self-test called undefined helpers:" >&2
    sed 's/^/      /' "$GUARD_MARKER" >&2
    failures=$((failures + 1))
fi
rm -f "$GUARD_MARKER"

if (( failures > 0 )); then
    echo "test_check_yamux_muxer: $failures assertion(s) failed." >&2
    exit 1
fi
echo "test_check_yamux_muxer: OK — all assertions passed."
