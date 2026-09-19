#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/test_check_dialable_hosts.sh
#
# Self-test for check_dialable_hosts.sh.
#
# The guard's whole job is to fail on a change nobody else notices, so
# the cases that matter are the two FAILING directions -- a feature
# turned on with the host list left behind, and a host list widened with
# the feature still off. A guard that only ever reported OK would read as
# coverage and be exactly as useful as none.
#
# The comment-dropping case is here because the real manifest's libp2p
# array contains a comment paragraph naming `dns`; without that filter
# the guard would read the feature as enabled and fail on the tree it
# ships in.
#
# Exit codes:
#   0  all assertions passed
#   1  one or more failed

set -uo pipefail

SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
UNDER_TEST="$SCRIPT_DIR/check_dialable_hosts.sh"
[[ -f "$UNDER_TEST" ]] || { echo "test: $UNDER_TEST not found" >&2; exit 1; }

failures=0
SANDBOX=""
cleanup() { [[ -n "$SANDBOX" && -d "$SANDBOX" ]] && rm -rf "$SANDBOX"; }
trap cleanup EXIT

pass() { echo "  ✓ $1"; }
fail() { echo "  ✗ $1" >&2; printf '%s\n' "${2:-}" | sed 's/^/      /' >&2
         failures=$((failures + 1)); }

# A throwaway tree with just the two files the guard reads, at the paths
# it reads them from.
run_against() {
    local features="$1" dialable="$2"
    SANDBOX="$(mktemp -d)"
    mkdir -p "$SANDBOX/tools/checks" "$SANDBOX/crates/config/profile-config/src"
    cp "$UNDER_TEST" "$SANDBOX/tools/checks/"
    {
        echo '[workspace.dependencies]'
        echo 'libp2p = { version = "0.56", default-features = false, features = ['
        printf '%s\n' "$features"
        echo '] }'
    } > "$SANDBOX/Cargo.toml"
    printf '%s\n' "$dialable" > "$SANDBOX/crates/config/profile-config/src/lib.rs"
    RUN_OUT="$( cd "$SANDBOX" && bash tools/checks/check_dialable_hosts.sh 2>&1 )"
    RUN_RC=$?
    rm -rf "$SANDBOX"; SANDBOX=""
}

assert_rc() {
    if [[ "$RUN_RC" == "$2" ]]; then pass "$1"
    else fail "$1 (expected exit $2, got $RUN_RC)" "$RUN_OUT"; fi
}

assert_contains() {
    if printf '%s' "$RUN_OUT" | grep -qF -- "$2"; then pass "$1"
    else fail "$1 (output did not contain: $2)" "$RUN_OUT"; fi
}

IP_ONLY='const DIALABLE_HOST_PROTOCOLS: [&str; 2] = ["ip4", "ip6"];'
WITH_DNS='const DIALABLE_HOST_PROTOCOLS: [&str; 4] = ["ip4", "ip6", "dns4", "dns6"];'

echo "check_dialable_hosts.sh"

# THE TREE AS IT SHIPS: no `dns` feature, no dns host dialable.
run_against '    "tcp",
    "noise",' "$IP_ONLY"
assert_rc "agreeing tree passes" 0
assert_contains "and says so" "agree"

# THE DIRECTION THAT WILL ACTUALLY HAPPEN: the feature lands and the
# refusal is left behind, so the validator refuses what the build can
# now dial.
run_against '    "tcp",
    "dns",' "$IP_ONLY"
assert_rc "feature on, host list behind -> fails" 1
assert_contains "and names the direction" "is ON, but 'dns4' is not in"

# THE OTHER DIRECTION: the host list widens with no transport under it,
# which is the silent-forget the refusal exists to stop.
run_against '    "tcp",
    "noise",' "$WITH_DNS"
assert_rc "host list ahead of the feature -> fails" 1
assert_contains "and names that direction too" "feature 'dns' is OFF"

# BOTH MOVED TOGETHER, which is what the lifting change must look like.
run_against '    "tcp",
    "dns",' "$WITH_DNS"
assert_rc "both moved together passes" 0

# A FEATURE NAMED ONLY IN A COMMENT IS NOT ENABLED. The real manifest
# discusses `dns` inside the array; read naively the guard would fail on
# the tree it ships in.
run_against '    "tcp",
    # `dns` is absent with no stage owning it, so a "dns4" address is
    # refused at validation.
    "noise",' "$IP_ONLY"
assert_rc "a commented-out feature is not read as enabled" 0

# INVOCATION PROBLEMS ARE 2, NOT A FINDING AND NOT A PASS.
#
# The first two are the ones that nearly got away. Under
# `set -euo pipefail` a `grep` matching nothing returns 1 and kills the
# script at the extraction, so a malformed input exited 1 -- SILENTLY,
# and 1 is the code this guard uses for "they disagree". A formatting
# change to the manifest would have been reported as a finding against
# the tree. Review finding on PR #108.
SANDBOX="$(mktemp -d)"
mkdir -p "$SANDBOX/tools/checks" "$SANDBOX/crates/config/profile-config/src"
cp "$UNDER_TEST" "$SANDBOX/tools/checks/"
printf '[workspace]\nmembers = []\n' > "$SANDBOX/Cargo.toml"
printf '%s\n' "$IP_ONLY" > "$SANDBOX/crates/config/profile-config/src/lib.rs"
RUN_OUT="$( cd "$SANDBOX" && bash tools/checks/check_dialable_hosts.sh 2>&1 )"; RUN_RC=$?
rm -rf "$SANDBOX"; SANDBOX=""
assert_rc "a manifest with no libp2p array exits 2, not 1" 2
assert_contains "and says which file it could not read" "found no libp2p feature array"

SANDBOX="$(mktemp -d)"
mkdir -p "$SANDBOX/tools/checks" "$SANDBOX/crates/config/profile-config/src"
cp "$UNDER_TEST" "$SANDBOX/tools/checks/"
{
    echo 'libp2p = { version = "0.56", features = ['
    echo '    "tcp",'
    echo '] }'
} > "$SANDBOX/Cargo.toml"
printf '// no such const here\n' > "$SANDBOX/crates/config/profile-config/src/lib.rs"
RUN_OUT="$( cd "$SANDBOX" && bash tools/checks/check_dialable_hosts.sh 2>&1 )"; RUN_RC=$?
rm -rf "$SANDBOX"; SANDBOX=""
assert_rc "a source with no DIALABLE_HOST_PROTOCOLS exits 2, not 1" 2
assert_contains "and names that one too" "found no DIALABLE_HOST_PROTOCOLS"

SANDBOX="$(mktemp -d)"
RUN_OUT="$( cd "$SANDBOX" && bash "$UNDER_TEST" 2>&1 )"; RUN_RC=$?
rm -rf "$SANDBOX"; SANDBOX=""
assert_rc "a tree with no manifest exits 2" 2

RUN_OUT="$( bash "$UNDER_TEST" --nonsense 2>&1 )"; RUN_RC=$?
assert_rc "an unknown argument exits 2" 2

RUN_OUT="$( bash "$UNDER_TEST" --root 2>&1 )"; RUN_RC=$?
assert_rc "--root with no value exits 2" 2

RUN_OUT="$( bash "$UNDER_TEST" --root /nonexistent-dir-for-this-test 2>&1 )"; RUN_RC=$?
assert_rc "--root on a directory that cannot be entered exits 2" 2

RUN_OUT="$( bash "$UNDER_TEST" --help 2>&1 )"; RUN_RC=$?
assert_rc "--help exits 0" 0
assert_contains "--help explains the coupling" "cannot read that"

# AND IT RUNS AGAINST THE REAL TREE, which is the case CI runs.
RUN_OUT="$( cd "$SCRIPT_DIR/../.." && bash tools/checks/check_dialable_hosts.sh 2>&1 )"
RUN_RC=$?
assert_rc "the real tree agrees" 0

if [[ "$failures" -gt 0 ]]; then
    echo "check_dialable_hosts self-test: $failures failure(s)" >&2
    exit 1
fi
echo "check_dialable_hosts self-test: all assertions passed."
