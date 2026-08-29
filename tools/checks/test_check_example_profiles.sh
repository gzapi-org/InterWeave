#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# Self-test for check_example_profiles.sh. A guard that cannot fail
# proves nothing, so each case builds a synthetic tree where the answer
# is known and asserts the exit code.

set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
CHECK="$HERE/check_example_profiles.sh"
failures=0

scaffold() {
    # $1 root, $2 max_entries value
    mkdir -p "$1/crates/config/profile-config/src" "$1/architecture/config/examples"
    printf 'pub const CACHE_MAX_PEERS: usize = 1_024;\n' \
        > "$1/crates/config/profile-config/src/lib.rs"
    printf 'discovery:\n  providers:\n    - type: peer-cache\n      config:\n        max_entries: %s\n' \
        "$2" > "$1/architecture/config/examples/p.yaml"
}

expect() {
    local name="$1" want="$2" root="$3"
    bash "$CHECK" --root "$root" >/dev/null 2>&1
    local got=$?
    if [ "$got" != "$want" ]; then
        echo "FAIL $name: wanted exit $want, got $got" >&2
        failures=$((failures + 1))
    else
        echo "ok   $name"
    fi
}

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

scaffold "$tmp/ok" 1024
expect "a value at the ceiling passes" 0 "$tmp/ok"

scaffold "$tmp/over" 4096
expect "a value past the ceiling fails" 1 "$tmp/over"

scaffold "$tmp/zero" 0
expect "a zero value fails" 1 "$tmp/zero"

# The ceiling is READ, not restated: a different constant moves the line.
scaffold "$tmp/moved" 4096
printf 'pub const CACHE_MAX_PEERS: usize = 8_192;\n' \
    > "$tmp/moved/crates/config/profile-config/src/lib.rs"
expect "the ceiling comes from the code, not a copy" 0 "$tmp/moved"

# A vanished constant is an invocation error, never a silent pass.
scaffold "$tmp/gone" 4096
printf 'pub const SOMETHING_ELSE: usize = 1;\n' \
    > "$tmp/gone/crates/config/profile-config/src/lib.rs"
expect "a moved constant is an invocation error" 2 "$tmp/gone"

mkdir -p "$tmp/noexamples/crates/config/profile-config/src"
printf 'pub const CACHE_MAX_PEERS: usize = 1_024;\n' \
    > "$tmp/noexamples/crates/config/profile-config/src/lib.rs"
expect "a missing examples directory is an invocation error" 2 "$tmp/noexamples"

# `--root` with no value must not hang; the failure mode is an infinite
# loop, so a bare assertion would hang the suite rather than fail it.
if timeout 5 bash "$CHECK" --root >/dev/null 2>&1; then
    echo "FAIL --root with no value: expected an invocation error" >&2
    failures=$((failures + 1))
else
    got=$?
    if [ "$got" = 124 ]; then
        echo "FAIL --root with no value: hung instead of failing" >&2
        failures=$((failures + 1))
    elif [ "$got" != 2 ]; then
        echo "FAIL --root with no value: wanted exit 2, got $got" >&2
        failures=$((failures + 1))
    else
        echo "ok   --root with no value is an invocation error"
    fi
fi

if bash "$CHECK" --help 2>&1 | grep -q 'check_example_profiles.sh'; then
    echo "ok   --help describes the script"
else
    echo "FAIL --help" >&2
    failures=$((failures + 1))
fi

if [ "$failures" -ne 0 ]; then
    echo "test_check_example_profiles: $failures failure(s)" >&2
    exit 1
fi
echo "test_check_example_profiles: OK — 8 cases."
