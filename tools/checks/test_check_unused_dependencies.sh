#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/test_check_unused_dependencies.sh
#
# Self-test for check_unused_dependencies.sh.
#
# Each case builds a SANDBOX cargo workspace and runs a copy of the guard
# there, because the guard's subject is the workspace it stands in. Every
# dependency is a PATH dependency inside the sandbox, so `cargo metadata`
# resolves without a registry and the suite runs offline.
#
# THE CASES THAT MATTER ARE THE REFUSALS: an unused dependency in a
# member must fail the guard with exit 1, and so must an unused
# DEV-dependency, which the guard's `--with-metadata` exists for -- the
# default mode does not read them. And a metadata failure must be exit 2:
# cargo-machete itself exits 0 on one. The scoping case proves
# the other half of the design: an unused dependency OUTSIDE the
# workspace (where the spikes and third_party/ live) is not judged.
#
# Exit codes:
#   0  all assertions passed
#   1  one or more failed
#   2  cargo-machete, cargo or jq is not installed, so none of this can
#      be exercised

set -uo pipefail

SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
UNDER_TEST="$SCRIPT_DIR/check_unused_dependencies.sh"
[[ -f "$UNDER_TEST" ]] || { echo "test: $UNDER_TEST not found" >&2; exit 1; }

for tool in cargo-machete cargo jq; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "test_check_unused_dependencies: $tool is not installed, so the guard cannot be exercised." >&2
        echo "  See check_unused_dependencies.sh --help, and run this again." >&2
        exit 2
    fi
done

failures=0
SANDBOX=""
cleanup() { [[ -n "$SANDBOX" && -d "$SANDBOX" ]] && rm -rf "$SANDBOX"; }
trap cleanup EXIT

pass() { echo "  ✓ $1"; }
fail() { echo "  ✗ $1" >&2; failures=$((failures + 1)); }

# crate <dir> <name> <deps-toml> <lib.rs body> [extra manifest text]
crate() {
    mkdir -p "$SANDBOX/$1/src"
    printf '[package]\nname = "%s"\nversion = "0.1.0"\nedition = "2021"\npublish = false\n\n[dependencies]\n%s\n%s\n' \
        "$2" "$3" "${5:-}" > "$SANDBOX/$1/Cargo.toml"
    printf '%s\n' "$4" > "$SANDBOX/$1/src/lib.rs"
}

# A workspace of two members, `app` and `util`, where `app` uses `util`,
# plus `util_extra` for the prefix case. Cases then change one thing.
fresh_sandbox() {
    cleanup
    SANDBOX="$(mktemp -d)"
    mkdir -p "$SANDBOX/tools/checks"
    cp "$UNDER_TEST" "$SANDBOX/tools/checks/"
    printf '[workspace]\nresolver = "2"\nmembers = ["crates/app", "crates/util", "crates/util_extra"]\n' > "$SANDBOX/Cargo.toml"
    crate crates/util util "" 'pub fn f() {}'
    crate crates/util_extra util_extra "" 'pub fn g() {}'
    crate crates/app app 'util = { path = "../util" }' 'pub fn h() { util::f(); }'
    lock
}

# `--locked` in the guard needs a lockfile; path dependencies resolve
# offline.
lock() { ( cd "$SANDBOX" && cargo generate-lockfile --offline --quiet ) || { echo "test: generate-lockfile failed" >&2; exit 1; }; }

guard_run() {
    GUARD_OUTPUT=$( cd "$SANDBOX" && bash tools/checks/check_unused_dependencies.sh "$@" 2>&1 )
    GUARD_STATUS=$?
}

# THE EXIT CODE, not merely non-zero: 1 is "found something", 2 is "could
# not look", and a refusal case that accepted either would pass on a guard
# that never ran cargo-machete at all.
expect_status() {
    local want="$1" label="$2"
    if [[ "$GUARD_STATUS" -eq "$want" ]]; then
        pass "$label"
    else
        fail "$label (expected exit $want, got $GUARD_STATUS)"
        printf '%s\n' "$GUARD_OUTPUT" | sed 's/^/      /' >&2
    fi
}

echo "check_unused_dependencies self-test"

fresh_sandbox
guard_run
expect_status 0 "a workspace whose members use what they declare passes"
if [[ "$GUARD_OUTPUT" == *"OK — 3 workspace members"* ]]; then
    pass "the OK line counts the members it judged"
else
    fail "the OK line does not say '3 workspace members': $GUARD_OUTPUT"
fi

fresh_sandbox
crate crates/app app 'util = { path = "../util" }
util_extra = { path = "../util_extra" }' 'pub fn h() { util::f(); }'
lock
guard_run
expect_status 1 "a member declaring a dependency it never uses is refused"
[[ "$GUARD_OUTPUT" == *util_extra* ]] && pass "the refusal names the dependency" \
    || fail "the refusal does not name util_extra"

# cargo-machete's default mode does not read [dev-dependencies]; the two
# real ones it missed on 07b5d4bd were both there.
fresh_sandbox
crate crates/app app 'util = { path = "../util" }

[dev-dependencies]
util_extra = { path = "../util_extra" }' 'pub fn h() { util::f(); }'
lock
guard_run
expect_status 1 "an unused DEV-dependency is refused (--with-metadata)"

fresh_sandbox
mkdir -p "$SANDBOX/spikes"
crate spikes/frozen frozen 'util = { path = "../../crates/util" }' 'pub fn s() {}'
printf '[workspace]\n' >> "$SANDBOX/spikes/frozen/Cargo.toml"
guard_run
expect_status 0 "an unused dependency outside the workspace members is not judged"

# The case above never reaches `cargo metadata`'s package list: nothing in
# the workspace depends on the spike. The repository's own non-members DO
# reach it -- `[patch.crates-io]` points two crates at third_party/ -- so
# only the member filter keeps them out (and every registry crate is in
# that list too). The same shape here: a patched,
# vendored crate that declares a dependency it never uses. Without the
# filter the guard hands it to cargo-machete and exits 1. Review finding
# F1 on #142.
fresh_sandbox
printf '\n[patch.crates-io]\nvendored = { path = "third_party/vendored" }\n' >> "$SANDBOX/Cargo.toml"
crate third_party/helper helper "" 'pub fn k() {}'
crate third_party/vendored vendored 'helper = { path = "../helper" }' 'pub fn v() {}'
sed -i 's/^version = "0.1.0"$/version = "1.0.0"/' "$SANDBOX/third_party/vendored/Cargo.toml"
# Each its own workspace root, so cargo-machete's metadata call resolves
# there -- as it does for the registry crates in the real graph, which a
# guard without the filter judged one by one. Without these tables the
# call fails and the guard refuses the skipped crate (exit 2), so the
# mutation would still go red, but through that refusal rather than
# through a finding; the tables keep this case proving the filter itself.
printf '\n[workspace]\n' >> "$SANDBOX/third_party/vendored/Cargo.toml"
printf '\n[workspace]\n' >> "$SANDBOX/third_party/helper/Cargo.toml"
crate crates/app app 'util = { path = "../util" }
vendored = "1"' 'pub fn h() { util::f(); vendored::v(); }'
lock
guard_run
expect_status 0 "a non-member in the dependency graph (a [patch] to vendored code) is not judged"

fresh_sandbox
crate crates/app app 'util = { path = "../util" }
util_extra = { path = "../util_extra" }' 'pub fn h() { util::f(); }' '
[package.metadata.cargo-machete]
ignored = ["util_extra"]'
lock
guard_run
expect_status 0 "an ignore entry in the member's manifest is honoured"

fresh_sandbox
guard_run crates/app
expect_status 2 "an argument is refused, not read as a narrower subject"

fresh_sandbox
printf 'this is not toml\n' > "$SANDBOX/Cargo.toml"
guard_run
expect_status 2 "an unreadable workspace is exit 2, never a pass"

# The silent fallback: a path dependency that is gone makes cargo's
# metadata fail, and `cargo-machete --with-metadata` then exits 0.
fresh_sandbox
crate crates/app app 'util = { path = "../util" }
gone = { path = "../gone" }' 'pub fn h() { util::f(); }'
guard_run
expect_status 2 "a metadata failure is exit 2, not cargo-machete's silent pass"

# The same fallback where `--no-deps` would not notice: a registry
# dependency, used, that cargo cannot resolve offline. The member list
# reads fine without resolution; the dev-dependency judgement does not.
fresh_sandbox
crate crates/app app 'util = { path = "../util" }
interweave-no-such-crate = "1"' 'pub fn h() { util::f(); interweave_no_such_crate::f(); }'
GUARD_OUTPUT=$( cd "$SANDBOX" && CARGO_NET_OFFLINE=true bash tools/checks/check_unused_dependencies.sh 2>&1 )
GUARD_STATUS=$?
expect_status 2 "an unresolvable registry dependency is exit 2, even though the members can be listed"

# cargo-machete failing is exit 2, not a pass: a stub standing in for it.
fresh_sandbox
stub="$SANDBOX/stub"
mkdir -p "$stub"
printf '#!/bin/sh\nexit 2\n' > "$stub/cargo-machete"
chmod +x "$stub/cargo-machete"
GUARD_OUTPUT=$( cd "$SANDBOX" && PATH="$stub:$PATH" bash tools/checks/check_unused_dependencies.sh 2>&1 )
GUARD_STATUS=$?
expect_status 2 "cargo-machete's own failure is exit 2, never a pass"

# cargo-machete's own report of a crate it could not read: 0.9.2 prints
# this on stderr, calls the crate clean and exits 0. A stub stands in,
# because the real tool only does it when cargo's metadata fails between
# the guard's check and its own call.
fresh_sandbox
stub="$SANDBOX/stub"
mkdir -p "$stub"
printf '#!/bin/sh\necho "error when handling crates/app/Cargo.toml: cargo metadata exited with an error" >&2\nexit 0\n' > "$stub/cargo-machete"
chmod +x "$stub/cargo-machete"
GUARD_OUTPUT=$( cd "$SANDBOX" && PATH="$stub:$PATH" bash tools/checks/check_unused_dependencies.sh 2>&1 )
GUARD_STATUS=$?
expect_status 2 "a crate cargo-machete skipped as unreadable is exit 2, though it exits 0"

# A root package: cargo-machete would walk the whole tree from ".".
fresh_sandbox
printf '\n[package]\nname = "rootpkg"\nversion = "0.1.0"\nedition = "2021"\n' >> "$SANDBOX/Cargo.toml"
mkdir -p "$SANDBOX/src" && printf 'pub fn r() {}\n' > "$SANDBOX/src/lib.rs"
lock
guard_run
expect_status 2 "a root package is refused, never judged by walking the whole tree"

# cargo-machete absent: a PATH holding only what the guard needs before it
# looks for the tool.
fresh_sandbox
bindir="$SANDBOX/bin"
mkdir -p "$bindir"
for t in bash dirname cat; do ln -s "$(command -v "$t")" "$bindir/$t"; done
GUARD_OUTPUT=$( cd "$SANDBOX" && PATH="$bindir" bash tools/checks/check_unused_dependencies.sh 2>&1 )
GUARD_STATUS=$?
expect_status 2 "a missing cargo-machete is exit 2, never a pass"

if [[ $failures -gt 0 ]]; then
    echo "test_check_unused_dependencies: $failures assertion(s) failed" >&2
    exit 1
fi
echo "test_check_unused_dependencies: all assertions passed"
