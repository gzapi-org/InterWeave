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
# MEASURED BEFORE THE RUN, not after. Taking it after a first run makes
# the assertion unfalsifiable: a guard that rewrites the lock does so
# during that run, and the second sees an already-fresh file. A review
# named the mutation that slipped through -- adding the printed remedy
# to the stale branch rewrites the lock AND still reports STALE, so
# every assertion here passed. That is the sibling guard's recorded
# incident exactly (review, PR #107).
before="$( md5sum < "$SANDBOX/spikes/spike-test/harness/Cargo.lock" )"
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
# THE GUARD MUST NOT REWRITE THE LOCK IT IS JUDGING. `--locked` refuses
# to update it, and that is the file's central claim.
after="$( md5sum < "$SANDBOX/spikes/spike-test/harness/Cargo.lock" )"
if [[ "$before" == "$after" ]]; then pass "and leaves the lock byte-identical"
else fail "and leaves the lock byte-identical — the guard rewrote it"; fi
rm -rf "$SANDBOX"; SANDBOX=""

# TWO LOCKS, ONE STALE: the loop accumulates rather than stopping at the
# first, and both are named. With a single lock per case nothing pins
# that -- `stale=1` or a `break` would pass every case above.
new_sandbox
( cd "$SANDBOX/spikes/spike-test/harness" && cargo generate-lockfile -q --offline 2>/dev/null )
mkdir -p "$SANDBOX/spikes/spike-two/harness/src"
cat > "$SANDBOX/spikes/spike-two/harness/Cargo.toml" <<'MANIFEST'
[package]
name = "spike-two-harness"
version = "0.0.0"
edition = "2021"

[workspace]

[dependencies]
MANIFEST
echo 'fn main() {}' > "$SANDBOX/spikes/spike-two/harness/src/main.rs"
( cd "$SANDBOX/spikes/spike-two/harness" && cargo generate-lockfile -q --offline 2>/dev/null )
mkdir -p "$SANDBOX/spikes/spike-two/sibling/src"
cat > "$SANDBOX/spikes/spike-two/sibling/Cargo.toml" <<'MANIFEST'
[package]
name = "spike-two-sibling"
version = "0.0.0"
edition = "2021"
MANIFEST
echo '' > "$SANDBOX/spikes/spike-two/sibling/src/lib.rs"
cat >> "$SANDBOX/spikes/spike-two/harness/Cargo.toml" <<'MANIFEST'
spike-two-sibling = { path = "../sibling" }
MANIFEST
# BOTH stale, not one: with a single stale lock among two, a `break`
# after the first and an accumulating loop are indistinguishable. Both
# named is what pins that the loop keeps going.
mkdir -p "$SANDBOX/spikes/spike-test/sibling/src"
cat > "$SANDBOX/spikes/spike-test/sibling/Cargo.toml" <<'MANIFEST'
[package]
name = "spike-test-sibling"
version = "0.0.0"
edition = "2021"
MANIFEST
echo '' > "$SANDBOX/spikes/spike-test/sibling/src/lib.rs"
cat >> "$SANDBOX/spikes/spike-test/harness/Cargo.toml" <<'MANIFEST'
spike-test-sibling = { path = "../sibling" }
MANIFEST
run_guard
assert_rc "two stale locks fail" 1
assert_contains "the first is named" "spikes/spike-test/harness/Cargo.lock is STALE"
assert_contains "and so is the second" "spikes/spike-two/harness/Cargo.lock is STALE"
rm -rf "$SANDBOX"; SANDBOX=""

# CARGO ABSENT IS EXIT 2, NOT A SILENT PASS. This is the file's most
# emphatic comment and nothing tested it: mutating its `exit 2` to
# `exit 0` broke no assertion.
new_sandbox
( cd "$SANDBOX/spikes/spike-test/harness" && cargo generate-lockfile -q --offline 2>/dev/null )
# Pointed at a cargo that is not there, rather than by emptying PATH --
# the script needs find, sed and the rest, so an empty PATH tests the
# harness and not the guard. That is the whole reason the guard honours
# $CARGO. (An earlier version of this comment also claimed it pins the
# invocation to the toolchain. It is the other way round under rustup:
# PATH holds the shim, which reads `rust-toolchain.toml`; $CARGO is
# cargo's own binary and bypasses it. No `rust-toolchain*` exists under
# `spikes/`, so the two agree here -- review, PR #107.)
RUN_OUT="$( cd "$SANDBOX" && CARGO=/nonexistent/cargo bash tools/checks/check_spike_locks.sh 2>&1 )"
RUN_RC=$?
assert_rc "cargo absent exits 2, not 0" 2
assert_contains "and names cargo as what is missing" "cargo is not available"
rm -rf "$SANDBOX"; SANDBOX=""

# A CARGO FAILURE THAT IS NOT ABOUT THE LOCK IS EXIT 2, NOT A FINDING.
# This is the branch the whole "environment problem" fix consists of,
# and nothing reached it: every other case either resolves or is stale,
# so `elif true` -- which is the original defect verbatim, every failure
# reported as STALE with a remedy pointing at the diff -- passed the
# entire suite (review, PR #107).
new_sandbox
( cd "$SANDBOX/spikes/spike-test/harness" && cargo generate-lockfile -q --offline 2>/dev/null )
printf '\n[[[ this is not toml\n' >> "$SANDBOX/spikes/spike-test/harness/Cargo.toml"
run_guard
assert_rc "a cargo failure that is not the lock exits 2" 2
assert_contains "and says it could not ask rather than blaming the lock" \
    "cargo failed for another reason"
rm -rf "$SANDBOX"; SANDBOX=""

# THE GIT ENUMERATION IS THE BRANCH CI TAKES, and no case reached it:
# every sandbox is a mktemp outside any work tree, so all of them took
# the `find` fallback. A wrong pathspec would have been a silent
# zero-lock pass in production with the suite fully green.
new_sandbox
( cd "$SANDBOX/spikes/spike-test/harness" && cargo generate-lockfile -q --offline 2>/dev/null )
git -C "$SANDBOX" init -q
git -C "$SANDBOX" add -f spikes/spike-test/harness/Cargo.lock >/dev/null 2>&1
run_guard
assert_rc "inside a checkout, the tracked lock is found and checked" 0
assert_contains "and counted" "1 committed spike lock"

# ...AND A CHECKOUT WITH HARNESSES BUT NO TRACKED LOCK IS EXIT 2, which
# is the failure mode the OK line exists to prevent: it looks exactly
# like success.
git -C "$SANDBOX" rm --cached -q spikes/spike-test/harness/Cargo.lock
run_guard
assert_rc "harnesses present but no tracked lock exits 2" 2
assert_contains "and says which way it could be wrong" "neither is a pass"
rm -rf "$SANDBOX"; SANDBOX=""

# INVOCATION PROBLEMS ARE 2 TOO.
RUN_OUT="$( bash "$UNDER_TEST" --nonsense 2>&1 )"; RUN_RC=$?
assert_rc "an unknown argument exits 2" 2
RUN_OUT="$( bash "$UNDER_TEST" --root 2>&1 )"; RUN_RC=$?
assert_rc "--root with no value exits 2" 2
RUN_OUT="$( bash "$UNDER_TEST" --root /nonexistent-dir-for-this-test 2>&1 )"; RUN_RC=$?
assert_rc "--root on a directory that cannot be entered exits 2" 2

# AND `--root` IS EXERCISED AT ALL, rather than only the copied-in form
# every other case uses.
new_sandbox
( cd "$SANDBOX/spikes/spike-test/harness" && cargo generate-lockfile -q --offline 2>/dev/null )
RUN_OUT="$( bash "$UNDER_TEST" --root "$SANDBOX" 2>&1 )"; RUN_RC=$?
assert_rc "--root <dir> checks that tree" 0
assert_contains "and reports its lock" "1 committed spike lock"
rm -rf "$SANDBOX"; SANDBOX=""
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
