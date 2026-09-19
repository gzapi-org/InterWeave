#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/check_dialable_hosts.sh
#
# >>> help
# Do the host protocols `profile-config` calls dialable match the
# transports the root manifest actually builds?
#
# `profile-config` REFUSES a configured `/dns4` or `/dns6` host
# (`ConfigError::AddressHostNotBuilt`), because this build has no `dns`
# transport: such a dial fails `MultiaddrNotSupported`, which is
# classified structural, so the address is dropped from the book rather
# than retried. A configured bootstrap peer would be silently never
# contacted.
#
# WHY THE ROOT MANIFEST IS THE WHOLE ANSWER. Cargo features are
# ADDITIVE, so `libp2p = { workspace = true, features = ["dns"] }` in
# any member crate would turn the transport on for the whole graph while
# the root array stayed as it is. Every member today writes a bare
# `libp2p = { workspace = true }`, and this guard refuses one that does
# not -- otherwise its own reading of the root array is only a guess
# (review, PR #108).
#
# WHY A GUARD AND NOT A TEST. The fact lives in the ROOT manifest's
# libp2p feature array; `profile-config` is a neutral contract crate and
# CLAUDE.md §4 forbids it a libp2p dependency, so it cannot read that
# array and no test in it can fail when the array changes:
#
#   `dns` OFF and `dns4` dialable -> the validator accepts an address
#                                    this build forgets on first use.
#   `dns` ON  and `dns4` refused  -> the validator refuses an address
#                                    this build may now reach, and the
#                                    six shipped examples that name DNS
#                                    hosts stay permanently filtered
#                                    with nothing saying why.
#
# WHAT THIS GUARD DELIBERATELY DOES NOT ASK, and why the gap is stated
# here rather than papered over. Enabling the `dns` feature only makes
# the transport AVAILABLE; the Swarm builder must still wrap the base
# transport in it, and today it does not. So a change that turns the
# feature on, widens `DIALABLE_HOST_PROTOCOLS`, and forgets the builder
# would pass this guard.
#
# An earlier version of this file tried to close that by searching
# `crates/transport/libp2p/src/runtime/mod.rs` for the construction.
# Five review rounds found seven ways that search was wrong -- four
# shapes that satisfied it while the builder constructed nothing (a
# `#[cfg(test)]` item, a rustfmt-split grouped `use`, a `/* */` block
# comment quoting real code, a string literal quoting real code) and
# three real constructions it missed (`.with_dns_config(`,
# `.with_other_transport(a_dns_builder)`, a macro). Each round fixed the
# shape it was shown and the next found another, because "does this code
# call this function" is a question about types and `grep` answers a
# question about text. The search is gone rather than patched an eighth
# time: a check that can be satisfied by a comment is worse than no
# check, because it reads as coverage.
#
# WHAT MUST REPLACE IT, in the change that enables `dns`: a test that
# builds the real transport and asserts the error kind for a
# `/dns4/.../tcp/...` dial -- `TransportError::MultiaddrNotSupported`
# while the transport is unbuilt, something else once it is. That is
# unfoolable by any lexical shape, and it is where this repository puts
# such questions. It needs the builder factored out of
# `SubstrateRuntime`, which is why it is an obligation recorded in the
# plan and not a line in this file.
#
# NOT CHECKED HERE. Whether a `/dns4` dial then SUCCEEDS -- resolution,
# the resolver's configuration, what happens to a name with no record.
# That is the transport's business.
#
# Exit codes:
#   0  the two agree, and no member manifest adds libp2p features
#   1  they disagree, or a member does -- the message says which
#   2  invocation problem -- a file is missing or unreadable; a file
#      does not carry the declaration this reads out of it (the libp2p
#      feature array, `DIALABLE_HOST_PROTOCOLS`, `[workspace].members`);
#      a listed member has no readable manifest; a temporary file
#      cannot be created; an unknown argument; `--root` without a
#      value; or a root that cannot be entered. Never a finding, and
#      never a pass, which is why the extractions below do not let
#      `errexit` turn an unparseable input into a 1.
# <<< help

set -euo pipefail

root="."
while [ $# -gt 0 ]; do
    case "$1" in
        --help | -h)
            sed -n '/^# >>> help$/,/^# <<< help$/p' "$0" |
                sed '1d;$d;s/^# \{0,1\}//'
            exit 0
            ;;
        --root)
            shift
            [ $# -gt 0 ] || {
                echo "check_dialable_hosts: --root needs a directory" >&2
                exit 2
            }
            root="$1"
            ;;
        *)
            echo "check_dialable_hosts: unknown argument: $1" >&2
            exit 2
            ;;
    esac
    shift
done

cd "$root" 2>/dev/null || {
    echo "check_dialable_hosts: cannot enter $root" >&2
    exit 2
}

manifest="Cargo.toml"
source_file="crates/config/profile-config/src/lib.rs"
for f in "$manifest" "$source_file"; do
    [ -r "$f" ] || {
        echo "check_dialable_hosts: cannot read $f" >&2
        exit 2
    }
done

# The libp2p feature array, from the `libp2p = { ... features = [` line
# to its closing `]`. Comment lines inside it are dropped, so a feature
# merely DISCUSSED in a comment is not read as enabled. The array does
# carry comment paragraphs -- the `cbor` note, the Stage 11 one -- and
# none of them currently quotes a feature name, so deleting the filter
# would change nothing TODAY; it is there for the paragraph that does.
# An earlier version of this comment claimed the array already named
# `dns` in a comment, which is false: every `dns` in the manifest is
# part of `mdns` and every one is outside the array (review, PR #108).
#
# `|| true` is not decoration. Under `set -euo pipefail` a `grep` that
# matches nothing returns 1, the assignment takes the pipeline's status,
# and the script dies right here -- silently, with exit 1, which this
# file documents as "they disagree". A malformed manifest would have
# been reported as a FINDING, and the empty-result check below would
# never have run. Both extractions carry it for the same reason, and
# `a manifest with no libp2p array exits 2, not 1` and its sibling for
# the source file are the tests.
features="$(
    awk '
        # The array ends at a line starting with `]` -- or, when the
        # whole declaration is on ONE line (legal TOML), at that line.
        # Without the second clause `inside` never cleared and the
        # extraction bled into every dependency below, turning
        # `has_feature` into a read of the entire manifest; measured on
        # a reconstructed one-line manifest, which picked up tokio\047s
        # features. Review, PR #108.
        # The declaration is on ONE line when that line closes the
        # feature array; anything after it -- a trailing `}` , a
        # trailing `# comment` -- is irrelevant. Keying on a line-final
        # `}` instead meant a trailing comment reinstated the bleed
        # exactly (measured, review PR #108).
        /^[[:space:]]*libp2p[[:space:]]*=[[:space:]]*\{/ {
            inside = 1
            single = /features[[:space:]]*=[[:space:]]*\[.*\]/
        }
        inside && !/^[[:space:]]*#/ { print }
        # The multi-line terminator is the array-closing bracket at any
        # indent. Anchored at column zero it missed the `    ] }` many
        # formatters produce, and the read bled on through the manifest.
        inside && (/^[[:space:]]*\]/ || single) { inside = 0; single = 0 }
    ' "$manifest" | grep -oE '"[a-z0-9-]+"' | tr -d '"' | sort -u || true
)"
[ -n "$features" ] || {
    echo "check_dialable_hosts: found no libp2p feature array in $manifest" >&2
    exit 2
}

dialable="$(
    grep -oE 'const DIALABLE_HOST_PROTOCOLS: \[&str; [0-9]+\] = \[[^]]*\]' "$source_file" |
        grep -oE '"[a-z0-9]+"' | tr -d '"' | sort -u || true
)"
[ -n "$dialable" ] || {
    echo "check_dialable_hosts: found no DIALABLE_HOST_PROTOCOLS in $source_file" >&2
    exit 2
}

has_feature() { printf '%s\n' "$features" | grep -qx "$1"; }
is_dialable() { printf '%s\n' "$dialable" | grep -qx "$1"; }

# THE FEATURE IS NOT THE TRANSPORT, which is the half a first version of
# this guard missed. `libp2p`'s `dns` feature only makes the resolving
# transport AVAILABLE; the Swarm builder still has to wrap the base
# transport in it, and today it does not -- it is `.with_tcp(...)`
# alone, plus the relay client's transport when one is configured,
# neither of which resolves a name. So a change that turned the feature
# on and widened
# `DIALABLE_HOST_PROTOCOLS` and forgot the builder would pass a guard
# that read the manifest alone, while the validator started accepting
# addresses that still fail `MultiaddrNotSupported` and are forgotten:
# exactly the regression this file exists to prevent, one step further
# along. Review finding on PR #108.
#
# COMMENTS ARE STRIPPED FIRST, and the construction must come BEFORE
# any `with_relay_client`. The first is the false positive a review
# found: a prose line naming `with_dns` satisfied a bare grep. The
# second is the shared-chain property -- this crate forks the builder
# into a relay and a no-relay branch, and a construction inside one
# would leave the other resolving nothing. libp2p's own phase types
# already forbid that (`with_dns` exists on `DnsPhase`, `QuicPhase` and
# `OtherTransportPhase`, all of which precede `RelayPhase`, so the code
# would not compile), which is why this is cheap insurance rather than
# the load-bearing check -- but insurance that costs two lines and
# survives a builder redesign is worth having. Review, PR #108.
fail=0

# One row per (libp2p feature, the builder call that actually constructs
# it, the host protocol it makes dialable). `tcp` is not a row: it is a
# TRANSPORT protocol, and the host half of an address is what this guard
# is about.
check_row() {
    local feature="$1" host="$2"
    if is_dialable "$host" && ! has_feature "$feature"; then
        echo "check_dialable_hosts: '$host' is in DIALABLE_HOST_PROTOCOLS, but libp2p" >&2
        echo "  feature '$feature' is OFF -- profile-config would accept an address this" >&2
        echo "  build fails structural and then FORGETS. Remove '$host' or enable" >&2
        echo "  '$feature'." >&2
        fail=1
    elif has_feature "$feature" && ! is_dialable "$host"; then
        echo "check_dialable_hosts: libp2p feature '$feature' is ON, but '$host' is not in" >&2
        echo "  DIALABLE_HOST_PROTOCOLS -- either profile-config still refuses an address" >&2
        echo "  this build can dial, or the feature was enabled without the Swarm builder" >&2
        echo "  being wrapped in the transport. THIS GUARD CANNOT TELL THOSE APART: see" >&2
        echo "  its --help for why, and for the test that must land with the builder." >&2
        fail=1
    fi
}

# One row per (libp2p feature, the host protocol it governs).
check_row dns dns4
check_row dns dns6

# AND NOBODY ADDS A FEATURE BEHIND THE ROOT'S BACK. Without this the
# rows above read one declaration and call it the answer, while a member
# crate could enable a transport for the whole graph.
#
# A manifest that declares its own `[workspace]` is a SEPARATE graph --
# the spike harnesses each do, with their own lockfile -- so its
# features reach no shipped binary and it is not this guard's business.
# That is the test for exclusion rather than a path list, because a path
# list goes stale the first time a spike moves.
# THE MEMBERS ARE READ FROM `[workspace].members`, which is the
# authoritative roster -- not from a `find`, which swept in the
# vendored `third_party/` tree and the spike harnesses and then
# complained about a dev-dependency of a crate this repository does not
# author. Only `[dependencies]` and `[target.*.dependencies]` count:
# a dev-dependency is not compiled into any shipped binary.
#
# NOT COVERED, and said plainly rather than left to be discovered.
# A member enabling a feature on a `libp2p-*` SUB-CRATE rather than on
# the facade: no route from there to `libp2p::dns` was found, and none
# was ruled out. And the vendored `third_party/` tree, which
# `[patch.crates-io]` compiles into this graph: it is outside
# `[workspace].members` and so outside this scan. Its manifests depend
# on `libp2p-*` sub-crates rather than on the facade today, which is
# the only reason that gap is not live.
#
# COMMENT LINES ARE DROPPED BEFORE THE TERMINATOR IS LOOKED FOR. The
# list carries seven comment paragraphs today and an `[ADR-0034]`-style
# reference in one of them would have ended the read there, leaving
# every member below it silently unscanned. Reproduced, review PR #108.
members="$(
    awk '/^[[:space:]]*members[[:space:]]*=/ { inside = 1 }
         inside && /^[[:space:]]*#/ { next }
         inside { print }
         inside && /\]/ { exit }' "$manifest" |
        grep -oE '"[^"]+"' | tr -d '"' || true
)"
# A MISSING ROSTER IS EXIT 2, NOT A PASS -- but an EMPTY one is a
# legitimate workspace with nothing to scan, so the two are told apart
# by whether the key is there at all. Without this the whole member
# check went inert on an indented `members = [`, on a terminator
# reached early, or on a rename, and the guard still printed OK with a
# third of its job not done: the silent-drop shape the other two
# extractions already refuse.
if ! grep -qE '^[[:space:]]*members[[:space:]]*=' "$manifest"; then
    echo "check_dialable_hosts: found no [workspace].members in $manifest" >&2
    echo "  -- the member scan has no roster to work from." >&2
    exit 2
fi
members_adding=""
scan_out="$( mktemp )" || {
    # EXIT 2, NOT ERREXIT'S 1. An assignment takes its command
    # substitution\047s status, so an unwritable or full TMPDIR would have
    # killed the script with 1 -- the code this file uses for "they
    # disagree" (review, PR #108).
    echo "check_dialable_hosts: cannot create a temporary file" >&2
    exit 2
}
trap 'rm -f "$scan_out"' EXIT
printf '%s\n' "$members" | while IFS= read -r member; do
    [ -n "$member" ] || continue
    m="$member/Cargo.toml"
    if [ ! -r "$m" ]; then
        # A LISTED MEMBER WITH NO READABLE MANIFEST IS REPORTED, not
        # skipped: a glob entry or a moved crate would otherwise take
        # itself out of the scan without saying so.
        echo "check_dialable_hosts: $manifest lists $member, but $m is not readable" >&2
        echo "  -- the member scan cannot speak for it." >&2
        exit 2
    fi
    hit="$(
        awk '
            # Any table ending in dependencies.libp2p] -- plain, or
            # target-scoped, whose spec carries quotes and parentheses
            # and so cannot be spelled as a character class. Dev- and
            # build-dependencies are excluded: neither is compiled into
            # a shipped binary.
            # F8: the negations are applied to the header WITHOUT its
            # comment, or `[dependencies.libp2p] # dev-dependencies are
            # below` would suppress a real production feature silently.
            /^[[:space:]]*\[.*dependencies\.libp2p\][[:space:]]*(#.*)?$/ {
                section = $0
                bare = $0; sub(/#.*$/, "", bare)
                if (bare !~ /dev-dependencies/ && bare !~ /build-dependencies/) {
                    intable = 1; header = NR ": " $0
                } else {
                    intable = 0
                }
                next
            }
            # EVERY table header updates the section -- including the
            # libp2p one above, which sets it before taking `next`, or
            # the claim would be false for exactly the tables this cares
            # about. The inline and dotted branches below consult it. Without that they
            # were context-free, so an inline
            # `libp2p = { workspace = true, features = ["dns"] }` under
            # `[dev-dependencies]` FAILED the guard -- a false positive
            # that blocks CI on a DNS-specific test and tells the author
            # to enable the feature in production instead (measured,
            # review PR #108). The table branch already excluded those
            # kinds; these two did not.
            /^[[:space:]]*\[/ { section = $0; intable = 0 }
            intable && /^[[:space:]]*features[[:space:]]*=/ {
                print header " -> " NR ": " $0; intable = 0; next
            }
            (section !~ /dev-dependencies/ && section !~ /build-dependencies/) &&
            (/^[[:space:]]*libp2p[[:space:]]*=[[:space:]]*\{.*features/ ||
             /^[[:space:]]*libp2p\.features[[:space:]]*=/) { print NR ": " $0 }
            # A member\047s OWN feature turning on a dependency feature:
            # `[features]` with `default = ["libp2p/dns"]`. This is the
            # ordinary Cargo way to do it and so the one most likely to
            # be written, and the first version of this scan -- which
            # called itself complete -- did not look for it at all.
            # NOT ON A COMMENT LINE. Every other extraction in this
            # file drops them, and this branch did not -- so a member
            # manifest documenting the rule ("never write
            # features = [\"libp2p/dns\"] here") failed the guard. That is
            # the third time in this file a prose line naming the
            # forbidden thing satisfied a bare grep (review, PR #108).
            !/^[[:space:]]*#/ && /"libp2p\/[a-z0-9-]+"/ { print NR ": " $0 }
        ' "$m" || true
    )"
    # `if`, not `[ -n ... ] &&`: the latter makes the loop body's
    # status 1 for every member with no hit, which is the loop's status,
    # which the `||` below then reads as a failure.
    if [ -n "$hit" ]; then
        printf '%s\n' "$m:$hit"
    fi
done > "$scan_out" || exit $?
members_adding="$( cat "$scan_out" )"

if [ -n "$members_adding" ]; then
    echo "check_dialable_hosts: a member manifest adds libp2p features of its own:" >&2
    printf '%s\n' "$members_adding" | sed 's/^/  /' >&2
    echo "  Cargo features are additive, so this enables a transport for the whole" >&2
    echo "  graph while the root array -- the only one the rows above read -- stays" >&2
    echo "  as it is. Move the feature to the root array." >&2
    fail=1
fi

if [ "$fail" -ne 0 ]; then
    exit 1
fi

echo "check_dialable_hosts: OK — the manifest's libp2p features and profile-config's dialable hosts agree, and no member adds features of its own."
exit 0
