#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/test_check_spike_locks.sh
#
# Self-test for check_spike_locks.sh.
#
# The guard's whole value is the POSITIVE case: a lock that no longer
# resolves must be reported. A guard that cannot fail reports OK forever
# and reads as coverage -- which is the state the spike locks were
# actually in, undetected across two stages, until this guard was
# written.
#
# The sandbox builds real cargo workspaces rather than fixtures, because
# the question the guard asks is cargo's (`--locked` refuses to update a
# lock) and a mocked answer would prove nothing about it. Each case is
# self-contained: a tiny crate with no dependencies, so nothing is
# fetched from the network.
#
# Exit codes:
#   0  all assertions passed
#   1  one or more failed
#   2  cargo is unavailable, so the guard's own question cannot be posed

set -uo pipefail

SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
UNDER_TEST="$SCRIPT_DIR/check_spike_locks.sh"
[[ -f "$UNDER_TEST" ]] || { echo "test: $UNDER_TEST not found" >&2; exit 1; }

if ! command -v cargo >/dev/null 2>&1; then
    echo "test_check_spike_locks: cargo unavailable; the guard cannot be exercised." >&2
    exit 2
fi

failures=0
SANDBOX=""
cleanup() { [[ -n "$SANDBOX" && -d "$SANDBOX" ]] && rm -rf "$SANDBOX"; }
trap cleanup EXIT

pass() { echo "  ✓ $1"; }
fail() { echo "  ✗ $1" >&2; printf '%s\n' "${2:-}" | sed 's/^/      /' >&2
         failures=$((failures + 1)); }

# A throwaway repository holding one spike harness, laid out as the real
# tree lays them out: spikes/<name>/harness/{Cargo.toml,Cargo.lock,src}.
new_sandbox() {
    SANDBOX="$(mktemp -d)"
    mkdir -p "$SANDBOX/tools/checks" "$SANDBOX/spikes/spike-test/harness/src"
    cp "$UNDER_TEST" "$SANDBOX/tools/checks/"
    cat > "$SANDBOX/spikes/spike-test/harness/Cargo.toml" <<'EOF'
[package]
name = "spike-test-harness"
version = "0.0.0"
edition = "2021"

[dependencies]
EOF
    echo 'fn main() {}' > "$SANDBOX/spikes/spike-test/harness/src/main.rs"
}

run_guard() {
    RUN_OUT="$(cd "$SANDBOX" && bash tools/checks/check_spike_locks.sh 2>&1)"
    RUN_RC=$?
}

assert_rc() {
    if [[ "$RUN_RC" -eq "$2" ]]; then pass "$1"
    else fail "$1 — expected exit $2, got $RUN_RC" "$RUN_OUT"; fi
}
assert_contains() {
    if [[ "$RUN_OUT" == *"$2"* ]]; then pass "$1"
    else fail "$1 — output lacked '$2'" "$RUN_OUT"; fi
}

echo "check_spike_locks.sh — a committed spike lock that stopped resolving"

# A lock that matches its manifest resolves: the guard passes and says
# how many it looked at, so a zero-lock pass cannot be mistaken for a
# real one.
new_sandbox
( cd "$SANDBOX/spikes/spike-test/harness" && cargo generate-lockfile -q --offline 2>/dev/null )
run_guard
assert_rc "a lock that matches its manifest passes" 0
assert_contains "and says how many it checked" "1 committed spike lock"
rm -rf "$SANDBOX"; SANDBOX=""

# THE CASE THE GUARD EXISTS FOR: the manifest gained a dependency the
# committed lock does not name -- exactly the shape a root-manifest
# change produces in a harness that path-depends on production crates.
# A dependency that is itself in the lock would not do: the lock must be
# INCOMPLETE for the build it now describes.
new_sandbox
( cd "$SANDBOX/spikes/spike-test/harness" && cargo generate-lockfile -q --offline 2>/dev/null )
mkdir -p "$SANDBOX/spikes/spike-test/sibling/src"
cat > "$SANDBOX/spikes/spike-test/sibling/Cargo.toml" <<'EOF'
[package]
name = "spike-test-sibling"
version = "0.0.0"
edition = "2021"
EOF
echo '' > "$SANDBOX/spikes/spike-test/sibling/src/lib.rs"
cat >> "$SANDBOX/spikes/spike-test/harness/Cargo.toml" <<'EOF'
spike-test-sibling = { path = "../sibling" }
EOF
run_guard
assert_rc "a lock missing a package the manifest needs FAILS" 1
assert_contains "and names the lock that is stale" "spikes/spike-test/harness/Cargo.lock"
assert_contains "and says what to do about it" "cargo metadata --format-version 1"
# AND WARNS OFF THE DESTRUCTIVE FORM BY NAME. The guard used to print
# `cargo generate-lockfile` as the remedy, which rewrites the lock from
# scratch -- measured at eighty-odd packages moved on these three -- so
# following the guard's own advice destroyed the pinning it exists to
# protect. This assertion is what stops that text coming back.
assert_contains "and warns off the destructive form" "Do NOT run"
rm -rf "$SANDBOX"; SANDBOX=""

# A harness with no committed lock pins nothing and so cannot drift:
# reporting it would train a reader to ignore this guard's output.
new_sandbox
run_guard
assert_rc "a harness with no lock is not a failure" 0
assert_contains "and says so rather than passing silently" "no committed spike locks"
rm -rf "$SANDBOX"; SANDBOX=""

# A tree with no spikes at all: the same reasoning one level up.
new_sandbox
rm -rf "$SANDBOX/spikes"
run_guard
assert_rc "a tree with no spikes/ is not a failure" 0
assert_contains "and says why it had nothing to do" "no spikes/ directory"
rm -rf "$SANDBOX"; SANDBOX=""

if (( failures > 0 )); then
    echo "test_check_spike_locks: $failures assertion(s) failed." >&2
    exit 1
fi
echo "test_check_spike_locks: OK — all assertions passed."
