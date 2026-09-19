#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/test_check_yamux_muxer.sh
#
# Self-test for check_yamux_muxer.sh.
#
# The guard is the only mechanism that can see this: yamux 0.12.1's
# remote-panic denial of service (GHSA-vxx9-2994-q338) has no RustSec
# advisory, so `cargo-deny` reports clean and always will. A guard that
# cannot fail is worse than none -- it reads as coverage -- so the case
# that matters is the POSITIVE one.
#
# The guard reads `cargo tree`, so these cases drive it through a stub
# `cargo` on PATH rather than by building a real graph: the assertion
# under test is how the guard READS the graph, and a real resolution
# would take a network and minutes to say the same thing.
#
# Exit codes:
#   0  all assertions passed
#   1  one or more failed

set -uo pipefail

SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
UNDER_TEST="$SCRIPT_DIR/check_yamux_muxer.sh"
[[ -f "$UNDER_TEST" ]] || { echo "test: $UNDER_TEST not found" >&2; exit 1; }

failures=0
SANDBOX=""
cleanup() { [[ -n "$SANDBOX" && -d "$SANDBOX" ]] && rm -rf "$SANDBOX"; }
trap cleanup EXIT

pass() { echo "  ✓ $1"; }
fail() { echo "  ✗ $1" >&2; printf '%s\n' "${2:-}" | sed 's/^/      /' >&2
         failures=$((failures + 1)); }

# A sandbox whose `cargo tree` prints the graph this case is about.
run_against() {
    SANDBOX="$(mktemp -d)"
    mkdir -p "$SANDBOX/tools/checks" "$SANDBOX/bin"
    cp "$UNDER_TEST" "$SANDBOX/tools/checks/"
    cat > "$SANDBOX/bin/cargo" <<EOF
#!/usr/bin/env bash
if [[ "\$1" == "tree" ]]; then
    cat <<'GRAPH'
$1
GRAPH
    exit 0
fi
exit 1
EOF
    chmod +x "$SANDBOX/bin/cargo"
    RUN_OUT="$(cd "$SANDBOX" && PATH="$SANDBOX/bin:$PATH" bash tools/checks/check_yamux_muxer.sh 2>&1)"
    RUN_RC=$?
    rm -rf "$SANDBOX"; SANDBOX=""
}

# A sandbox whose `cargo tree` REFUSES to show the edge unless it was
# asked for every target. A stub printing the same graph either way
# could not tell the two invocations apart, so the flag is made the
# difference.
run_against_target_only() {
    SANDBOX="$(mktemp -d)"
    mkdir -p "$SANDBOX/tools/checks" "$SANDBOX/bin"
    cp "$UNDER_TEST" "$SANDBOX/tools/checks/"
    cat > "$SANDBOX/bin/cargo" <<'STUB'
#!/usr/bin/env bash
if [[ "$1" == "tree" ]]; then
    for a in "$@"; do
        if [[ "$a" == "all" ]]; then
            printf 'yamux v0.12.1\n'
            exit 0
        fi
    done
    printf 'libp2p-yamux v0.48.0\n'
    exit 0
fi
exit 1
STUB
    chmod +x "$SANDBOX/bin/cargo"
    RUN_OUT="$(cd "$SANDBOX" && PATH="$SANDBOX/bin:$PATH" bash tools/checks/check_yamux_muxer.sh 2>&1)"
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

echo "check_yamux_muxer.sh — the vulnerable muxer line in the build graph"

# THE CASE THE GUARD EXISTS FOR.
run_against "interweave-transport-libp2p v0.0.0
libp2p-yamux v0.47.0
yamux v0.12.1
yamux v0.13.10"
assert_rc "the 0.12 line in the graph FAILS" 1
assert_contains "and names the version found" "yamux v0.12.1"
assert_contains "and says cargo-deny cannot see it" "no RustSec advisory"

# The state this repository is in after the libp2p 0.57 bump.
run_against "interweave-transport-libp2p v0.0.0
libp2p-yamux v0.48.0
yamux v0.14.0"
assert_rc "a graph with only 0.14 passes" 0
assert_contains "and says which version is present" "v0.14.0"

# THE SUBSTRING TRAP, measured on the real graph while this guard was
# rewritten: `libp2p-yamux` ends in `yamux`, and that crate has had a
# 0.12 line of its own. Matching the name whole is what keeps the
# wrapper from being reported as the muxer.
run_against "interweave-transport-libp2p v0.0.0
libp2p-yamux v0.12.0
yamux v0.14.0"
assert_rc "libp2p-yamux 0.12 is NOT the muxer and does not fail the guard" 0
assert_contains "and the wrapper is not reported as present" "v0.14.0"

# A graph with no yamux at all: nothing to say, and it says so rather
# than passing silently.
run_against "interweave-transport-libp2p v0.0.0"
assert_rc "a graph with no yamux passes" 0
assert_contains "and says none is present" "none present"

# A TARGET-SPECIFIC EDGE IS STILL SEEN. `cargo tree` defaults to the
# host, and this job runs on ubuntu -- so an android-only dependency on
# yamux 0.12 was invisible to the one mechanism that can see this crate
# at all. The stub answers differently for the two invocations, so this
# case fails if the flag is dropped (review, PR #109).
run_against_target_only
assert_rc "an edge only another target has is still found" 1
assert_contains "and the vulnerable line is named" "yamux v0.12.1"

if (( failures > 0 )); then
    echo "test_check_yamux_muxer: $failures assertion(s) failed." >&2
    exit 1
fi
echo "test_check_yamux_muxer: OK — all assertions passed."
