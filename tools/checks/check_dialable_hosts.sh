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
#      carry the declaration this reads out of it, an unknown argument,
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
        /^libp2p = \{/ { inside = 1 }
        inside && /^\]/ { inside = 0 }
        inside && !/^[[:space:]]*#/ { print }
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
# transport in it, and today it does not -- it is `.with_tcp(...)` and
# nothing else. So a change that turned the feature on and widened
# `DIALABLE_HOST_PROTOCOLS` and forgot the builder would pass a guard
# that read the manifest alone, while the validator started accepting
# addresses that still fail `MultiaddrNotSupported` and are forgotten:
# exactly the regression this file exists to prevent, one step further
# along. Review finding on PR #108.
builds_transport() { grep -qE "$1" "$builder_file"; }

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

if [ "$fail" -ne 0 ]; then
    exit 1
fi

echo "check_dialable_hosts: OK — the manifest's features, the Swarm builder's transports and profile-config's dialable hosts agree."
exit 0
