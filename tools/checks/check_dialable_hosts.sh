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
# libp2p feature array and in the Swarm builder; `profile-config` is a
# neutral contract crate and CLAUDE.md §4 forbids it a libp2p
# dependency, so it can read neither and no test in it can fail when
# either changes. The coupling is real and invisible in every direction:
#
#   `dns` OFF and `dns4` dialable  -> the validator accepts an address
#                                     this build forgets on first use.
#   feature ON, builder still      -> the same, one step further along:
#   TCP-only, `dns4` dialable         the feature makes the transport
#                                     AVAILABLE and the builder must
#                                     still wrap the base transport in
#                                     it. Today it does not.
#   both ON and `dns4` refused     -> the validator refuses an address
#                                     this build can now reach, and the
#                                     six shipped examples that name DNS
#                                     hosts stay permanently filtered
#                                     with nothing saying why.
#
# The second is the one that will actually happen: the refusal is
# written to LIFT in the change that turns `dns` on, and nothing else in
# the tree would notice if that change forgot. This guard is what
# notices.
#
# NOT CHECKED HERE. Whether a `/dns4` dial then SUCCEEDS -- resolution,
# the resolver's configuration, what happens to a name with no record.
# That is the transport's business and `tests/` proves it. This asks
# only whether the three declarations agree with each other.
#
# Exit codes:
#   0  the three agree
#   1  they disagree, and the message says which direction
#   2  invocation problem -- a file is missing, unreadable or does not
#      carry the declaration this reads out of it (including a Swarm
#      builder that has moved or been renamed, which empties the region
#      the construction check reads), an unknown argument,
#      `--root` without a value, or a root that cannot be entered. Never
#      a finding, and never a pass, which is why the extractions below
#      do not let `errexit` turn an unparseable input into a 1.
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
builder_file="crates/transport/libp2p/src/runtime/mod.rs"
for f in "$manifest" "$source_file" "$builder_file"; do
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
        /^libp2p[[:space:]]*=[[:space:]]*\{/ { inside = 1; single = /\}[[:space:]]*$/ }
        inside && !/^[[:space:]]*#/ { print }
        inside && (/^\]/ || single) { inside = 0; single = 0 }
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
builds_transport() {
    local region construct relay
    # THE PRODUCTION BUILDER REGION, not the module. Stripping comments
    # and `use` lines was not enough: removing a `#[cfg(test)]`
    # ATTRIBUTE leaves the item under it, so a test helper constructing
    # the transport still satisfied a module-wide grep while the
    # production Swarm stayed TCP-only (measured, review PR #108). This
    # file carries five `#[cfg(test)]` modules and its siblings carry
    # test swarm builders of exactly that shape.
    #
    # So the search is bounded to the statement that builds the real
    # Swarm -- from `SwarmBuilder::with_existing_identity` to the
    # `GatedSwarm::new` that consumes it, which spans the shared chain
    # and both of its branches and ends before any test module.
    region="$(
        awk '/SwarmBuilder::with_existing_identity/ { inside = 1 }
             inside { print }
             inside && /GatedSwarm::new/ { exit }' "$builder_file" |
            sed -e 's|//.*$||' \
                -e '/^[[:space:]]*\(#\[[^]]*\][[:space:]]*\)*use /d' \
                -e '/^[[:space:]]*#\[/d'
    )"
    if [ -z "$region" ]; then
        echo "check_dialable_hosts: found no Swarm builder in $builder_file" >&2
        echo "  (looked for SwarmBuilder::with_existing_identity .. GatedSwarm::new)." >&2
        echo "  The builder moved or was renamed; this guard must be pointed at it" >&2
        echo "  again rather than left reporting on a region that is not there." >&2
        exit 2
    fi
    construct="$( printf '%s\n' "$region" | grep -nE "$1" | head -1 | cut -d: -f1 )"
    [ -n "$construct" ] || return 1
    # AND BEFORE ANY `with_relay_client`, which is the shared-chain
    # property: a construction inside one branch would leave the other
    # resolving nothing. libp2p\047s phase types already forbid that --
    # `with_dns` lives on `DnsPhase`, `QuicPhase` and
    # `OtherTransportPhase`, all of which precede `RelayPhase` -- so
    # this is insurance that survives a builder redesign, not the
    # reason the property holds.
    relay="$( printf '%s\n' "$region" | grep -n 'with_relay_client' | head -1 | cut -d: -f1 )"
    [ -z "$relay" ] || [ "$construct" -lt "$relay" ]
}

fail=0

# One row per (libp2p feature, the builder call that actually constructs
# it, the host protocol it makes dialable). `tcp` is not a row: it is a
# TRANSPORT protocol, and the host half of an address is what this guard
# is about.
check_row() {
    local feature="$1" construction="$2" host="$3"
    if is_dialable "$host"; then
        # CALLED DIALABLE, so both the feature and the construction must
        # be there -- either one missing is an address the validator
        # accepts and the build cannot reach.
        if ! has_feature "$feature"; then
            echo "check_dialable_hosts: '$host' is in DIALABLE_HOST_PROTOCOLS, but libp2p" >&2
            echo "  feature '$feature' is OFF -- profile-config would accept an address this" >&2
            echo "  build fails structural and then FORGETS. Remove '$host' or enable" >&2
            echo "  '$feature'." >&2
            fail=1
        fi
        if ! builds_transport "$construction"; then
            echo "check_dialable_hosts: '$host' is in DIALABLE_HOST_PROTOCOLS, but" >&2
            echo "  $builder_file does not construct the transport for it (no match for" >&2
            echo "  /$construction/). The feature only makes it AVAILABLE; the Swarm" >&2
            echo "  builder must wrap the base transport in it, or the address still" >&2
            echo "  fails MultiaddrNotSupported and is forgotten." >&2
            fail=1
        fi
    elif has_feature "$feature" && builds_transport "$construction"; then
        # BUILT AND NOT CALLED DIALABLE: the refusal outlived its reason.
        echo "check_dialable_hosts: libp2p feature '$feature' is ON and" >&2
        echo "  $builder_file constructs the transport, but '$host' is not in" >&2
        echo "  DIALABLE_HOST_PROTOCOLS -- profile-config still refuses an address this" >&2
        echo "  build can now dial. Add '$host' there and delete the refusal's prose." >&2
        fail=1
    fi
}

# `with_dns` is the `SwarmBuilder` step; `dns::Transport` covers building
# the resolver directly. Either constructs it, so either satisfies the
# row -- this asks whether the transport is CONSTRUCTED, not how.
check_row dns 'with_dns|dns::[A-Za-z_:]*Transport' dns4
check_row dns 'with_dns|dns::[A-Za-z_:]*Transport' dns6

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
# NOT COVERED, and said plainly rather than left to be discovered: a
# member enabling a feature on a `libp2p-*` SUB-CRATE rather than on
# the facade. No such route to `libp2p::dns` was found, but none was
# ruled out either.
members="$(
    awk '/^members[[:space:]]*=/ { inside = 1 }
         inside { print }
         inside && /\]/ { exit }' "$manifest" |
        grep -oE '"[^"]+"' | tr -d '"' || true
)"
members_adding=""
for member in $members; do
    m="$member/Cargo.toml"
    [ -r "$m" ] || continue
    hit="$(
        awk '
            # Any table ending in dependencies.libp2p] -- plain, or
            # target-scoped, whose spec carries quotes and parentheses
            # and so cannot be spelled as a character class. Dev- and
            # build-dependencies are excluded: neither is compiled into
            # a shipped binary.
            /^[[:space:]]*\[.*dependencies\.libp2p\][[:space:]]*$/ &&
            !/dev-dependencies/ && !/build-dependencies/ {
                intable = 1; header = NR ": " $0; next
            }
            # EVERY table header updates the section, and the inline
            # and dotted branches below consult it. Without that they
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
        ' "$m" || true
    )"
    [ -n "$hit" ] && members_adding="$members_adding$m:$hit"$'\n'
done

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

echo "check_dialable_hosts: OK — the manifest's features, the Swarm builder's transports and profile-config's dialable hosts agree."
exit 0
