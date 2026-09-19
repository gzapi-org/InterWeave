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
# The comment-dropping case is SYNTHETIC, and saying so matters: the
# real manifest's array carries comment paragraphs but none of them
# quotes a feature name, so the filter changes nothing on the tree today
# and this case is what would catch the paragraph that does. An earlier
# version of this header claimed the real array names `dns` in a
# comment; it does not (review, PR #108).
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
# The guard bounds its search to the PRODUCTION builder statement --
# `SwarmBuilder::with_existing_identity` through `GatedSwarm::new` --
# so every sandbox has to carry those markers or the guard correctly
# reports that it cannot find the builder at all.
write_builder() {
    cat > "$1/crates/transport/libp2p/src/runtime/mod.rs" <<RS
let builder = libp2p::SwarmBuilder::with_existing_identity(keypair)
    .with_tokio()
    $2;
let mut swarm = GatedSwarm::new(swarm);

#[cfg(test)]
mod tests {
    // A test helper that constructs the transport must NOT satisfy the
    // guard: it sits past GatedSwarm::new and outside the region.
    fn helper() { let _ = dns::tokio::Transport::system(base); }
}
RS
}

# The third argument is the Swarm builder's transport chain: the guard
# reads whether the DNS transport is CONSTRUCTED, not only whether its
# feature is on, so every case has to say which.
run_against() {
    local features="$1" dialable="$2" builder="${3:-.with_tcp(tcp::Config::default())}"
    SANDBOX="$(mktemp -d)"
    mkdir -p "$SANDBOX/tools/checks" "$SANDBOX/crates/config/profile-config/src" \
             "$SANDBOX/crates/transport/libp2p/src/runtime"
    cp "$UNDER_TEST" "$SANDBOX/tools/checks/"
    {
        echo '[workspace]'
        echo 'members = ["crates/config/profile-config", "crates/transport/libp2p"]'
        echo ''
        echo '[workspace.dependencies]'
        echo 'libp2p = { version = "0.56", default-features = false, features = ['
        printf '%s\n' "$features"
        echo '] }'
    } > "$SANDBOX/Cargo.toml"
    printf 'libp2p = { workspace = true }\n' \
        > "$SANDBOX/crates/config/profile-config/Cargo.toml"
    printf 'libp2p = { workspace = true }\n' \
        > "$SANDBOX/crates/transport/libp2p/Cargo.toml"
    printf '%s\n' "$dialable" > "$SANDBOX/crates/config/profile-config/src/lib.rs"
    write_builder "$SANDBOX" "$builder"
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

# THE HOST LIST WIDENS WITH NO TRANSPORT UNDER IT, which is the
# silent-forget the refusal exists to stop. The direction that will
# actually happen -- the transport lands and the refusal is left behind
# -- is further down, under THE REFUSAL OUTLIVING ITS REASON, because
# it now needs all three inputs set rather than two.
run_against '    "tcp",
    "noise",' "$WITH_DNS"
assert_rc "host list ahead of the feature -> fails" 1
assert_contains "and names that direction too" "feature 'dns' is OFF"

# THE HALF A FIRST VERSION OF THIS GUARD MISSED: the feature is on and
# the host list widened, but the builder is still TCP-only -- so the
# validator accepts an address that still fails `MultiaddrNotSupported`
# and is forgotten. This is the case the whole file exists for, one step
# further along than the manifest.
run_against '    "tcp",
    "dns",' "$WITH_DNS"
assert_rc "feature on and host dialable but the builder untouched -> fails" 1
assert_contains "and names the construction" "does not construct the transport for it"

# ALL THREE MOVED TOGETHER, which is what the lifting change must look
# like.
run_against '    "tcp",
    "dns",' "$WITH_DNS" '.with_tcp(tcp::Config::default()).with_dns()?'
assert_rc "all three moved together passes" 0

# AND THE BUILDER ALONE IS NOT ENOUGH EITHER, so neither side of the
# pair can vouch for the other.
run_against '    "tcp",' "$WITH_DNS" '.with_tcp(tcp::Config::default()).with_dns()?'
assert_rc "builder constructs it but the feature is off -> fails" 1
assert_contains "and names the feature" "feature 'dns' is OFF"

# PROSE IS NOT CONSTRUCTION, and neither is an import. A bare grep over
# the module matched all three of these -- and the first is a sentence
# this repository is very likely to write in that exact file.
# Each of these satisfied a search for the NAME at some point across
# four review rounds, in the shape the code would really be written in
# -- not a contrived one. The guard asks for CALL syntax now, which is
# what none of them has.
while IFS='|' read -r decoy label; do
    [ -n "$label" ] || continue
    run_against '    "tcp",
    "dns",' "$WITH_DNS" "$decoy"
    assert_rc "a $label mentioning the transport does not count as construction" 1
done <<'DECOYS'
// the builder is with_tcp alone and does not call with_dns yet|line comment
/* the builder does not call with_dns yet */|block comment
let msg = "call with_dns when the feature lands";|string literal
use libp2p::dns::tokio::Transport as DnsTransport;|single-line use
type Resolver = dns::tokio::Transport;|type alias
DECOYS

# A grouped `use` that rustfmt has split across lines -- the ordinary
# shape of the import a half-done DNS change would add.
run_against '    "tcp",
    "dns",' "$WITH_DNS" 'use libp2p::{
        dns::tokio::Transport,
        tcp,
    };'
assert_rc "a rustfmt-split grouped use does not count either" 1

# ...AND A cfg(test) MODULE BODY, which the region bound excludes: the
# helper written into every sandbox sits past GatedSwarm::new.

# A TEST-ONLY CONSTRUCTION IS NOT A PRODUCTION ONE. Stripping the
# `#[cfg(test)]` ATTRIBUTE leaves the item under it, so a helper
# building the transport satisfied a module-wide grep while the real
# Swarm stayed TCP-only. Every sandbox's mod.rs carries exactly such a
# helper past `GatedSwarm::new`; this asserts the guard does not see it
# (review, PR #108).
run_against '    "tcp",
    "dns",' "$WITH_DNS" '.with_tcp(tcp::Config::default())'
assert_rc "a #[cfg(test)] construction past the builder does not count" 1
assert_contains "and the guard says the builder does not construct it" \
    "does not construct the transport for it"

# AND THE GUARD SAYS SO WHEN THE BUILDER IS NOT WHERE IT LOOKS, rather
# than reporting on a region that is not there.
SANDBOX="$(mktemp -d)"
mkdir -p "$SANDBOX/tools/checks" "$SANDBOX/crates/config/profile-config/src" \
         "$SANDBOX/crates/transport/libp2p/src/runtime"
cp "$UNDER_TEST" "$SANDBOX/tools/checks/"
printf 'fn unrelated() {}\n' > "$SANDBOX/crates/transport/libp2p/src/runtime/mod.rs"
printf '%s\n' "$WITH_DNS" > "$SANDBOX/crates/config/profile-config/src/lib.rs"
{
    echo '[workspace]'
    echo 'members = []'
    echo 'libp2p = { version = "0.56", features = ['
    echo '    "dns",'
    echo '] }'
} > "$SANDBOX/Cargo.toml"
RUN_OUT="$( cd "$SANDBOX" && bash tools/checks/check_dialable_hosts.sh 2>&1 )"; RUN_RC=$?
rm -rf "$SANDBOX"; SANDBOX=""
assert_rc "a moved or renamed builder exits 2, not a silent pass" 2
assert_contains "and says what it looked for" "found no Swarm builder"

# THE OTHER CONSTRUCTION SHAPE. Building the resolver directly satisfies
# the row too -- the guard asks whether it is constructed, not how.
run_against '    "tcp",
    "dns",' "$WITH_DNS" 'let t = dns::tokio::Transport::system(base)?;'
assert_rc "a directly-built dns transport satisfies the row" 0

# THE REFUSAL OUTLIVING ITS REASON: everything is built and the host is
# still refused, which strands the six shipped examples.
run_against '    "tcp",
    "dns",' "$IP_ONLY" '.with_tcp(tcp::Config::default()).with_dns()?'
assert_rc "built but still refused -> fails" 1
assert_contains "and says the refusal outlived its reason" "is not in"

# A FEATURE NAMED ONLY IN A COMMENT IS NOT ENABLED. This case is
# SYNTHETIC, like the header says: the real manifest's array carries
# comment paragraphs but none of them quotes a feature name, so the
# filter changes nothing on the tree today. An earlier version of these
# three lines said the real array discusses `dns` inside itself, which
# is what the header was corrected for -- and it survived here, so the
# file asserted both (review, PR #108).
run_against '    "tcp",
    # `dns` is absent with no stage owning it, so a "dns4" address is
    # refused at validation.
    "noise",' "$IP_ONLY"
assert_rc "a commented-out feature is not read as enabled" 0

# A MEMBER CRATE MUST NOT ADD LIBP2P FEATURES, IN ANY SPELLING. Cargo
# unifies features identically across all four, so a guard that reads
# one of them reads the root array on a guess. The first version matched
# only the inline form at column zero; a review measured the other
# three (PR #108).
member_case() {
    SANDBOX="$(mktemp -d)"
    mkdir -p "$SANDBOX/tools/checks" "$SANDBOX/crates/config/profile-config/src" \
             "$SANDBOX/crates/transport/libp2p/src/runtime"
    cp "$UNDER_TEST" "$SANDBOX/tools/checks/"
    write_builder "$SANDBOX" '.with_tcp(tcp::Config::default())'
    printf '%s\n' "$IP_ONLY" > "$SANDBOX/crates/config/profile-config/src/lib.rs"
    {
        echo '[workspace]'
        echo 'members = ["crates/config/profile-config", "crates/transport/libp2p"]'
        echo 'libp2p = { version = "0.56", features = ['
        echo '    "tcp",'
        echo '] }'
    } > "$SANDBOX/Cargo.toml"
    printf 'libp2p = { workspace = true }\n' \
        > "$SANDBOX/crates/transport/libp2p/Cargo.toml"
    printf '%s\n' "$1" > "$SANDBOX/crates/config/profile-config/Cargo.toml"
    RUN_OUT="$( cd "$SANDBOX" && bash tools/checks/check_dialable_hosts.sh 2>&1 )"; RUN_RC=$?
    rm -rf "$SANDBOX"; SANDBOX=""
}

member_case 'libp2p = { workspace = true, features = ["dns"] }'
assert_rc "the inline form is caught" 1
member_case '  libp2p = { workspace = true, features = ["dns"] }'
assert_rc "the inline form INDENTED is caught" 1
member_case '[dependencies.libp2p]
workspace = true
features = ["dns"]'
assert_rc "the table form is caught" 1
assert_contains "and names the table" "dependencies.libp2p"
member_case '[target.'"'"'cfg(unix)'"'"'.dependencies.libp2p]
workspace = true
features = ["dns"]'
assert_rc "a target-scoped table is caught" 1
member_case 'libp2p.features = ["dns"]'
assert_rc "the dotted key is caught" 1

# ...AND WHAT MUST NOT FIRE. A dev-dependency is not compiled into any
# shipped binary, and a bare declaration is the shape every member uses.
member_case '[dev-dependencies.libp2p]
workspace = true
features = ["dns"]'
assert_rc "a dev-dependency table is not a shipped feature" 0
# THE INLINE FORM UNDER `[dev-dependencies]`, which the table branch
# excluded and the inline branch did not -- a false positive that blocks
# CI on a DNS-specific test and tells the author to enable the feature
# in production instead (review, PR #108).
member_case '[dev-dependencies]
libp2p = { workspace = true, features = ["dns"] }'
assert_rc "an INLINE dev-dependency is not a shipped feature either" 0
member_case '[build-dependencies]
libp2p.features = ["dns"]'
assert_rc "nor a dotted build-dependency" 0
member_case '[dependencies]
libp2p = { workspace = true, features = ["dns"] }'
assert_rc "but the same inline form under [dependencies] IS caught" 1
member_case 'libp2p = { workspace = true }'
assert_rc "the bare form every member uses passes" 0

# A ONE-LINE FEATURE ARRAY IS LEGAL TOML, and the extraction must stop
# at the end of that line. It did not: `inside` cleared only on a line
# starting with `]`, so the read bled into every dependency below and
# `has_feature` became a read of the whole manifest (review, PR #108).
SANDBOX="$(mktemp -d)"
mkdir -p "$SANDBOX/tools/checks" "$SANDBOX/crates/config/profile-config/src" \
         "$SANDBOX/crates/transport/libp2p/src/runtime"
cp "$UNDER_TEST" "$SANDBOX/tools/checks/"
write_builder "$SANDBOX" '.with_tcp(tcp::Config::default()).with_dns()?'
printf '%s\n' "$IP_ONLY" > "$SANDBOX/crates/config/profile-config/src/lib.rs"
{
    echo '[workspace]'
    echo 'members = []'
    echo 'libp2p = { version = "0.56", features = ["tcp", "noise"] }'
    echo 'other = { version = "1", features = ["dns"] }'
} > "$SANDBOX/Cargo.toml"
RUN_OUT="$( cd "$SANDBOX" && bash tools/checks/check_dialable_hosts.sh 2>&1 )"; RUN_RC=$?
rm -rf "$SANDBOX"; SANDBOX=""
# DISCRIMINATING because the builder CONSTRUCTS dns: with the bleed,
# `has_feature dns` reads true from the dependency below, the row's
# "built and not called dialable" arm fires and the guard exits 1. An
# earlier version of this case used a TCP-only builder, where neither
# arm fires either way -- it passed with the fix reverted and pinned
# nothing (review, PR #108).
assert_rc "a one-line array does not read the dependency below it" 0

# THE SAME, WITH A TRAILING COMMENT after the closing brace -- ordinary
# in a manifest that comments nearly every declaration, and it
# reinstated the bleed exactly.
SANDBOX="$(mktemp -d)"
mkdir -p "$SANDBOX/tools/checks" "$SANDBOX/crates/config/profile-config/src" \
         "$SANDBOX/crates/transport/libp2p/src/runtime"
cp "$UNDER_TEST" "$SANDBOX/tools/checks/"
write_builder "$SANDBOX" '.with_tcp(tcp::Config::default()).with_dns()?'
printf '%s\n' "$IP_ONLY" > "$SANDBOX/crates/config/profile-config/src/lib.rs"
{
    echo '[workspace]'
    echo 'members = []'
    echo 'libp2p = { version = "0.56", features = ["tcp"] } # the facade'
    echo 'other = { version = "1", features = ["dns"] }'
} > "$SANDBOX/Cargo.toml"
RUN_OUT="$( cd "$SANDBOX" && bash tools/checks/check_dialable_hosts.sh 2>&1 )"; RUN_RC=$?
rm -rf "$SANDBOX"; SANDBOX=""
assert_rc "nor does one with a trailing comment" 0

# A MISSING ROSTER IS EXIT 2, not a silent pass with the member scan
# inert. An EMPTY one is a legitimate workspace and must still pass --
# every case above uses `members = []`.
SANDBOX="$(mktemp -d)"
mkdir -p "$SANDBOX/tools/checks" "$SANDBOX/crates/config/profile-config/src" \
         "$SANDBOX/crates/transport/libp2p/src/runtime"
cp "$UNDER_TEST" "$SANDBOX/tools/checks/"
write_builder "$SANDBOX" '.with_tcp(tcp::Config::default())'
printf '%s\n' "$IP_ONLY" > "$SANDBOX/crates/config/profile-config/src/lib.rs"
printf '[workspace]\nlibp2p = { version = "0.56", features = ["tcp"] }\n' \
    > "$SANDBOX/Cargo.toml"
RUN_OUT="$( cd "$SANDBOX" && bash tools/checks/check_dialable_hosts.sh 2>&1 )"; RUN_RC=$?
assert_rc "a manifest with no members key exits 2" 2
assert_contains "and says the scan has no roster" "no roster to work from"

# A COMMENT CARRYING `]` INSIDE THE LIST must not end the read -- the
# real list carries seven comment paragraphs and an [ADR-0034]-style
# reference in one would have truncated it silently.
mkdir -p "$SANDBOX/m"
printf 'libp2p = { workspace = true, features = ["dns"] }\n' > "$SANDBOX/m/Cargo.toml"
{
    echo '[workspace]'
    echo 'members = ['
    echo '    # the opt-out gate [see ADR-0034] is why this one is here'
    echo '    "m",'
    echo ']'
    echo 'libp2p = { version = "0.56", features = ["tcp"] }'
} > "$SANDBOX/Cargo.toml"
RUN_OUT="$( cd "$SANDBOX" && bash tools/checks/check_dialable_hosts.sh 2>&1 )"; RUN_RC=$?
assert_rc "a comment containing ] does not truncate the roster" 1
assert_contains "and the member below it is still scanned" "m/Cargo.toml"

# A LISTED MEMBER WITH NO MANIFEST IS REPORTED, not skipped.
printf '[workspace]\nmembers = ["gone"]\nlibp2p = { version = "0.56", features = ["tcp"] }\n' \
    > "$SANDBOX/Cargo.toml"
RUN_OUT="$( cd "$SANDBOX" && bash tools/checks/check_dialable_hosts.sh 2>&1 )"; RUN_RC=$?
rm -rf "$SANDBOX"; SANDBOX=""
assert_rc "a listed member with no manifest exits 2" 2
assert_contains "and names it" "lists gone"

# INVOCATION PROBLEMS ARE 2, NOT A FINDING AND NOT A PASS.
#
# The first two are the ones that nearly got away. Under
# `set -euo pipefail` a `grep` matching nothing returns 1 and kills the
# script at the extraction, so a malformed input exited 1 -- SILENTLY,
# and 1 is the code this guard uses for "they disagree". A formatting
# change to the manifest would have been reported as a finding against
# the tree. Review finding on PR #108.
SANDBOX="$(mktemp -d)"
mkdir -p "$SANDBOX/tools/checks" "$SANDBOX/crates/config/profile-config/src" \
         "$SANDBOX/crates/transport/libp2p/src/runtime"
cp "$UNDER_TEST" "$SANDBOX/tools/checks/"
write_builder "$SANDBOX" '.with_tcp(tcp::Config::default())'
printf '[workspace]\nmembers = []\n' > "$SANDBOX/Cargo.toml"
printf '%s\n' "$IP_ONLY" > "$SANDBOX/crates/config/profile-config/src/lib.rs"
RUN_OUT="$( cd "$SANDBOX" && bash tools/checks/check_dialable_hosts.sh 2>&1 )"; RUN_RC=$?
rm -rf "$SANDBOX"; SANDBOX=""
assert_rc "a manifest with no libp2p array exits 2, not 1" 2
assert_contains "and says which file it could not read" "found no libp2p feature array"

SANDBOX="$(mktemp -d)"
mkdir -p "$SANDBOX/tools/checks" "$SANDBOX/crates/config/profile-config/src" \
         "$SANDBOX/crates/transport/libp2p/src/runtime"
cp "$UNDER_TEST" "$SANDBOX/tools/checks/"
write_builder "$SANDBOX" '.with_tcp(tcp::Config::default())'
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
assert_contains "--help explains the coupling" "it can read neither"

# AND IT RUNS AGAINST THE REAL TREE, which is the case CI runs.
RUN_OUT="$( cd "$SCRIPT_DIR/../.." && bash tools/checks/check_dialable_hosts.sh 2>&1 )"
RUN_RC=$?
assert_rc "the real tree agrees" 0

if [[ "$failures" -gt 0 ]]; then
    echo "check_dialable_hosts self-test: $failures failure(s)" >&2
    exit 1
fi
echo "check_dialable_hosts self-test: all assertions passed."
