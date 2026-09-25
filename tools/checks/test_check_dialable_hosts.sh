#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/test_check_dialable_hosts.sh
#
# Self-test for check_dialable_hosts.sh.
#
# The guard's job is to fail on a change nobody else notices, so the
# cases that matter are the FAILING ones -- a feature turned on with the
# host list left behind, a host list widened with the feature still off,
# and a member crate adding libp2p features of its own in any of the
# spellings Cargo accepts. A guard that only ever reported OK would read
# as coverage and be exactly as useful as none.
#
# The shapes that must NOT fire are here for the same reason: a false
# failure on a dev-dependency would block a DNS-specific test and send
# its author to enable the feature in production instead.
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

# A throwaway tree holding just what the guard reads, at the paths it
# reads them from. `$3` is an optional member manifest; when it is given
# the workspace lists one member, otherwise none.
run_against() {
    local features="$1" dialable="$2" member="${3:-}"
    SANDBOX="$(mktemp -d)"
    mkdir -p "$SANDBOX/tools/checks" "$SANDBOX/crates/config/profile-config/src"
    cp "$UNDER_TEST" "$SANDBOX/tools/checks/"
    printf '%s\n' "$dialable" > "$SANDBOX/crates/config/profile-config/src/lib.rs"
    {
        echo '[workspace]'
        if [ -n "$member" ]; then
            echo 'members = ["m"]'
        else
            echo 'members = []'
        fi
        echo ''
        echo '[workspace.dependencies]'
        echo 'libp2p = { version = "0.56", default-features = false, features = ['
        printf '%s\n' "$features"
        echo '] }'
    } > "$SANDBOX/Cargo.toml"
    if [ -n "$member" ]; then
        mkdir -p "$SANDBOX/m"
        printf '%s\n' "$member" > "$SANDBOX/m/Cargo.toml"
    fi
    RUN_OUT="$( cd "$SANDBOX" && bash tools/checks/check_dialable_hosts.sh 2>&1 )"
    RUN_RC=$?
    rm -rf "$SANDBOX"; SANDBOX=""
}

echo "check_dialable_hosts.sh"

# THE TREE AS IT SHIPS.
run_against '    "tcp",
    "noise",' "$IP_ONLY"
assert_rc "agreeing tree passes" 0
assert_contains "and says so" "agree"

# THE HOST LIST WIDENS WITH NO FEATURE UNDER IT: the validator would
# accept an address this build fails structural and then forgets.
run_against '    "tcp",' "$WITH_DNS"
assert_rc "host list ahead of the feature -> fails" 1
assert_contains "and names the direction" "feature 'dns' is OFF"

# THE FEATURE LANDS AND THE HOST LIST DOES NOT. The guard cannot tell a
# forgotten refusal from a forgotten builder, and says so rather than
# picking one.
run_against '    "tcp",
    "dns",' "$IP_ONLY"
assert_rc "feature on, host list behind -> fails" 1
assert_contains "and names both readings" "CANNOT TELL THOSE APART"

# BOTH MOVED TOGETHER, which is what the lifting change looks like.
run_against '    "tcp",
    "dns",' "$WITH_DNS"
assert_rc "both moved together passes" 0

# A FEATURE NAMED ONLY IN A COMMENT IS NOT ENABLED. Synthetic: the real
# array carries comment paragraphs but none quotes a feature name, so
# the filter changes nothing on the tree today and this case is what
# would catch the paragraph that does.
run_against '    "tcp",
    # `dns` is absent with no stage owning it, so a "dns4" address is
    # refused at validation.
    "noise",' "$IP_ONLY"
assert_rc "a commented-out feature is not read as enabled" 0

# A MEMBER CRATE MUST NOT ADD LIBP2P FEATURES. Cargo unifies features
# identically across every spelling, so a guard reading one of them
# reads the root array on a guess. These are the spellings checked --
# not "any spelling", which is a completeness claim no enumeration
# earns.
run_against '    "tcp",' "$IP_ONLY" 'libp2p = { workspace = true, features = ["dns"] }'
assert_rc "the inline form is caught" 1
assert_contains "and names the manifest" "m/Cargo.toml"
run_against '    "tcp",' "$IP_ONLY" '  libp2p = { workspace = true, features = ["dns"] }'
assert_rc "the inline form INDENTED is caught" 1
run_against '    "tcp",' "$IP_ONLY" '[dependencies.libp2p]
workspace = true
features = ["dns"]'
assert_rc "the table form is caught" 1
run_against '    "tcp",' "$IP_ONLY" '[dependencies.libp2p] # the facade
workspace = true
features = ["dns"]'
assert_rc "a table header with a trailing comment is caught" 1
run_against '    "tcp",' "$IP_ONLY" "[target.'cfg(unix)'.dependencies.libp2p]
workspace = true
features = [\"dns\"]"
assert_rc "a target-scoped table is caught" 1
run_against '    "tcp",' "$IP_ONLY" '[dependencies]
libp2p.features = ["dns"]'
assert_rc "the dotted key is caught" 1
run_against '    "tcp",' "$IP_ONLY" '[features]
default = ["libp2p/dns"]'
assert_rc "a member feature turning on a dependency feature is caught" 1

# ...AND WHAT MUST NOT FIRE. A dev- or build-dependency is compiled into
# no shipped binary, a bare declaration is what every member writes, and
# a comment naming the forbidden form is prose -- which satisfied a bare
# grep twice before in this file.
run_against '    "tcp",' "$IP_ONLY" '[dev-dependencies.libp2p]
workspace = true
features = ["dns"]'
assert_rc "a dev-dependency table is not a shipped feature" 0
run_against '    "tcp",' "$IP_ONLY" '[dev-dependencies]
libp2p = { workspace = true, features = ["dns"] }'
assert_rc "an INLINE dev-dependency is not either" 0
run_against '    "tcp",' "$IP_ONLY" '[build-dependencies]
libp2p.features = ["dns"]'
assert_rc "nor a dotted build-dependency" 0
run_against '    "tcp",' "$IP_ONLY" '[dependencies.libp2p] # dev-dependencies are below
workspace = true
features = ["dns"]'
assert_rc "a benign comment naming dev-dependencies does not suppress the catch" 1
run_against '    "tcp",' "$IP_ONLY" '[dependencies]
# never write features = ["libp2p/dns"] here -- the root array owns it
libp2p = { workspace = true }'
assert_rc "a comment naming the forbidden form is not a finding" 0
run_against '    "tcp",' "$IP_ONLY" 'libp2p = { workspace = true }'
assert_rc "the bare form every member uses passes" 0

# A ONE-LINE FEATURE ARRAY IS LEGAL TOML and the extraction must stop at
# the end of that line, with or without a trailing comment.
one_line_case() {
    SANDBOX="$(mktemp -d)"
    mkdir -p "$SANDBOX/tools/checks" "$SANDBOX/crates/config/profile-config/src"
    cp "$UNDER_TEST" "$SANDBOX/tools/checks/"
    printf '%s\n' "$IP_ONLY" > "$SANDBOX/crates/config/profile-config/src/lib.rs"
    {
        echo '[workspace]'
        echo 'members = []'
        printf '%s\n' "$1"
        echo 'other = { version = "1", features = ["dns"] }'
    } > "$SANDBOX/Cargo.toml"
    RUN_OUT="$( cd "$SANDBOX" && bash tools/checks/check_dialable_hosts.sh 2>&1 )"; RUN_RC=$?
    rm -rf "$SANDBOX"; SANDBOX=""
}
one_line_case 'libp2p = { version = "0.56", features = ["tcp"] }'
assert_rc "a one-line array does not read the dependency below it" 0
one_line_case 'libp2p = { version = "0.56", features = ["tcp"] } # the facade'
assert_rc "nor does one with a trailing comment" 0

# THE ROSTER. A MISSING key is exit 2 -- otherwise the member scan goes
# inert and the guard still prints OK with a third of its job undone.
# An EMPTY list is a legitimate workspace and passes; every case above
# relies on that.
roster_case() {
    SANDBOX="$(mktemp -d)"
    mkdir -p "$SANDBOX/tools/checks" "$SANDBOX/crates/config/profile-config/src" "$SANDBOX/m"
    cp "$UNDER_TEST" "$SANDBOX/tools/checks/"
    printf '%s\n' "$IP_ONLY" > "$SANDBOX/crates/config/profile-config/src/lib.rs"
    printf 'libp2p = { workspace = true, features = ["dns"] }\n' > "$SANDBOX/m/Cargo.toml"
    printf '%s\nlibp2p = { version = "0.56", features = ["tcp"] }\n' "$1" > "$SANDBOX/Cargo.toml"
    RUN_OUT="$( cd "$SANDBOX" && bash tools/checks/check_dialable_hosts.sh 2>&1 )"; RUN_RC=$?
    rm -rf "$SANDBOX"; SANDBOX=""
}
roster_case '[workspace]'
assert_rc "a manifest with no members key exits 2" 2
assert_contains "and says the scan has no roster" "no roster to work from"
roster_case '[workspace]
members = [
    # the opt-out gate [see ADR-0034] is why this one is here
    "m",
]'
assert_rc "a comment containing ] does not truncate the roster" 1
assert_contains "and the member below it is still scanned" "m/Cargo.toml"
roster_case '[workspace]
members = ["gone"]'
assert_rc "a listed member with no manifest exits 2" 2
assert_contains "and names it" "lists gone"

# INVOCATION PROBLEMS ARE 2, NOT A FINDING AND NOT A PASS. The first two
# are the ones that nearly got away: under `set -euo pipefail` a `grep`
# matching nothing returns 1 and killed the script at the extraction, so
# a malformed input exited 1 -- the code this guard uses for "they
# disagree".
SANDBOX="$(mktemp -d)"
mkdir -p "$SANDBOX/tools/checks" "$SANDBOX/crates/config/profile-config/src"
cp "$UNDER_TEST" "$SANDBOX/tools/checks/"
printf '%s\n' "$IP_ONLY" > "$SANDBOX/crates/config/profile-config/src/lib.rs"
printf '[workspace]\nmembers = []\n' > "$SANDBOX/Cargo.toml"
RUN_OUT="$( cd "$SANDBOX" && bash tools/checks/check_dialable_hosts.sh 2>&1 )"; RUN_RC=$?
rm -rf "$SANDBOX"; SANDBOX=""
assert_rc "a manifest with no libp2p array exits 2, not 1" 2
assert_contains "and says which file it could not read" "found no libp2p feature array"

SANDBOX="$(mktemp -d)"
mkdir -p "$SANDBOX/tools/checks" "$SANDBOX/crates/config/profile-config/src"
cp "$UNDER_TEST" "$SANDBOX/tools/checks/"
printf '// no such const here\n' > "$SANDBOX/crates/config/profile-config/src/lib.rs"
printf '[workspace]\nmembers = []\nlibp2p = { version = "0.56", features = ["tcp"] }\n' \
    > "$SANDBOX/Cargo.toml"
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
assert_contains "--help states the gap it does not check" "DELIBERATELY DOES NOT ASK"
# It names the FILE, not the error: `MultiaddrNotSupported` also appears in
# the paragraph explaining why the guard exists, so an assertion on it
# passed whether or not the help named any replacement at all.
assert_contains "--help names the test that replaced the construction search" "tests/dns_transport.rs"

# AND IT RUNS AGAINST THE REAL TREE, which is the case CI runs.
RUN_OUT="$( cd "$SCRIPT_DIR/../.." && bash tools/checks/check_dialable_hosts.sh 2>&1 )"
RUN_RC=$?
assert_rc "the real tree agrees" 0

if [[ "$failures" -gt 0 ]]; then
    echo "check_dialable_hosts self-test: $failures failure(s)" >&2
    exit 1
fi
echo "check_dialable_hosts self-test: all assertions passed."
