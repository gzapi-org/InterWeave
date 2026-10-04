#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# tools/checks/check_rustc_pin.sh
#
# >>> help
# Is the compiler cargo will use the one rust-toolchain.toml pins?
#
#   tools/checks/check_rustc_pin.sh
#
# rust-toolchain.toml pins `channel`, and rustup honours it. But a host
# without rustup builds with whatever rustc is on PATH: on a Fedora host
# that is the distribution's package, which matched the pin only because
# Fedora shipped 1.98.1, and a routine system update moves every login on
# the host to a new compiler at once with no commit here. CI builds with
# the pin, so the first sign would be a lint that fires on one side only.
# This asks the compiler and compares.
#
# WHAT IS ASKED: `${RUSTC:-rustc} --version`, run from the repository
# root — the compiler cargo itself would invoke, and where rustup reads
# the pin file. Its version token (`rustc 1.98.1 (…)` → `1.98.1`) must
# equal `channel` exactly when the channel is X.Y.Z, or start with X.Y.
# when it is X.Y. A pre-release (`1.98.1-beta.2`) is not the release.
#
# WHAT IT DOES NOT ASK: rustfmt's and clippy's versions, which come from
# the same toolchain under rustup but are separate packages on a
# distribution; and Cargo.toml's rust-version, kept equal to the channel
# by hand.
#
# Exit codes:
#   0  the compiler is the pinned one
#   1  it is not; both versions are printed
#   2  a failure to check: no rust-toolchain.toml, no `channel`, a
#      channel that names no version (`stable`, a nightly), or a rustc
#      that cannot be run or whose output carries no version
# <<< help

set -uo pipefail

ROOT="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )/../.." && pwd )"
cd "$ROOT" || exit 2
me="check_rustc_pin"

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
    sed -n '/^# >>> help$/,/^# <<< help$/p' "$0" | sed '1d;$d;s/^# \{0,1\}//'
    exit 0
fi

pin_file="rust-toolchain.toml"
[[ -f "$pin_file" ]] || { echo "$me: no $pin_file at the repository root" >&2; exit 2; }
channel="$(sed -n 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"\([^"]*\)".*$/\1/p' "$pin_file" | head -1)"
[[ -n "$channel" ]] || { echo "$me: $pin_file has no channel = \"…\"" >&2; exit 2; }
if [[ ! "$channel" =~ ^[0-9]+\.[0-9]+(\.[0-9]+)?$ ]]; then
    echo "$me: $pin_file's channel \"$channel\" names no version — the pin is X.Y.Z" >&2
    exit 2
fi

rustc_bin="${RUSTC:-rustc}"
if ! out="$("$rustc_bin" --version 2>&1)"; then
    echo "$me: \`$rustc_bin --version\` failed:" >&2
    printf '%s\n' "$out" | tail -3 >&2
    exit 2
fi
version="$(sed -n 's/^rustc \([^ ]*\).*$/\1/p' <<<"$out" | head -1)"
[[ -n "$version" ]] || { echo "$me: \`$rustc_bin --version\` printed no version: $out" >&2; exit 2; }

if [[ "$channel" =~ ^[0-9]+\.[0-9]+$ ]]; then
    [[ "$version" =~ ^${channel//./\\.}\.[0-9]+$ ]] && match=1
else
    [[ "$version" == "$channel" ]] && match=1
fi

if [[ -z "${match:-}" ]]; then
    echo "FAIL: the compiler is rustc $version; $pin_file pins $channel"
    echo "      ($(command -v "$rustc_bin" || echo "$rustc_bin"): $out)"
    echo "Install the pinned toolchain (rustup reads $pin_file), or bump the pin"
    echo "and Cargo.toml's rust-version together if the new compiler is the intent."
    exit 1
fi
echo "$me: rustc $version is the pinned $channel"
