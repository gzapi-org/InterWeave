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
# libp2p feature array; `profile-config` is a neutral contract crate and
# CLAUDE.md §4 forbids it a libp2p dependency, so it cannot read that
# array and no test in it can fail when the array changes. The coupling
# is real and invisible in both directions:
#
#   `dns` OFF and `dns4` dialable  -> the validator accepts an address
#                                     this build forgets on first use.
#   `dns` ON  and `dns4` refused   -> the validator refuses an address
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
# NOT CHECKED HERE. Whether libp2p's `dns` feature is what a `/dns4`
# dial needs at runtime -- that is the transport's business and
# `tests/` proves it. This asks only whether the two declarations agree.
#
# Exit codes:
#   0  the two agree
#   1  they disagree, and the message says which direction
#   2  invocation problem -- a file is missing or unreadable, an unknown
#      argument, `--root` without a value, or a root that cannot be
#      entered. Never a finding, and never a pass.
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
# merely DISCUSSED in a comment is not read as enabled -- the array has
# several such paragraphs, including one naming `dns` by name.
#
# `|| true` is not decoration. Under `set -euo pipefail` a `grep` that
# matches nothing returns 1, the assignment takes the pipeline's status,
# and the script dies right here -- silently, with exit 1, which this
# file documents as "they disagree". A malformed manifest would have
# been reported as a FINDING, and the empty-result check below would
# never have run. Both extractions carry it for the same reason, and
# `an unparseable manifest exits 2` is the test.
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

fail=0

# One row per (libp2p feature, the host protocols it makes dialable).
# `tcp` is not here: it is a TRANSPORT protocol, and the host half of an
# address is what this guard is about.
check_pair() {
    local feature="$1" host="$2"
    if has_feature "$feature" && ! is_dialable "$host"; then
        echo "check_dialable_hosts: libp2p feature '$feature' is ON, but '$host' is not in" >&2
        echo "  DIALABLE_HOST_PROTOCOLS -- profile-config still refuses an address this" >&2
        echo "  build can now dial. Add '$host' there and delete the refusal's prose." >&2
        fail=1
    elif ! has_feature "$feature" && is_dialable "$host"; then
        echo "check_dialable_hosts: '$host' is in DIALABLE_HOST_PROTOCOLS, but libp2p" >&2
        echo "  feature '$feature' is OFF -- profile-config would accept an address this" >&2
        echo "  build fails structural and then FORGETS. Remove '$host' or enable" >&2
        echo "  '$feature'." >&2
        fail=1
    fi
}

check_pair dns dns4
check_pair dns dns6

if [ "$fail" -ne 0 ]; then
    exit 1
fi

echo "check_dialable_hosts: OK — the manifest's transports and profile-config's dialable hosts agree."
exit 0
