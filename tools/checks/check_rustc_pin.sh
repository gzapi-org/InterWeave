#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# tools/checks/check_rustc_pin.sh
#
# >>> help
# Is the compiler cargo will use the one rust-toolchain.toml pins, and
# does Cargo.toml's rust-version state the same version?
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
# WHAT IS ASKED: the compiler cargo would invoke — `$RUSTC`, else
# `$CARGO_BUILD_RUSTC`, else `rustc` — run with --version from the
# repository root, where rustup reads the pin file. Its version token
# (`rustc 1.98.1 (…)` → `1.98.1`) must equal `channel` exactly when the
# channel is X.Y.Z, or be X.Y.<patch> when it is X.Y; a pre-release
# (`1.98.1-beta.2`) is neither (test_check_rustc_pin.sh, both pin forms).
# A `build.rustc` in a cargo config file also chooses the compiler, and
# this does not resolve one: a config in scope — the repository's
# .cargo/, a parent directory's, or $CARGO_HOME's — with any key spelled
# `rustc` (bare, quoted, dotted or in an inline table; rustc-wrapper is
# another key) is a failure to check, named. Loud rather than parsed:
# TOML spells one key several ways, and a missed spelling would pass
# silently. Cargo searches from the directory it runs in; this from the
# repository root, where `cargo xtask` and CI run — so a tracked cargo
# config below the root, which cargo would read from there, is a failure
# to check too.
#
# THE STATED MINIMUM: Cargo.toml's [workspace.package] rust-version must
# equal `channel` as written. A rust-version below the pin reads as a
# tested floor that nothing builds against; one above it refuses the
# pinned compiler. Both move together, and this is what says so.
#
# WHAT IT DOES NOT ASK: rustfmt's and clippy's versions, which come from
# the same toolchain under rustup but are separate packages on a
# distribution.
#
# Exit codes:
#   0  the compiler is the pinned one, and rust-version states the pin
#   1  either is not; both versions are printed
#   2  a failure to check: no rust-toolchain.toml, no `channel`, no
#      Cargo.toml or no rust-version in its [workspace.package], a
#      channel that names no version (`stable`, a nightly), a cargo
#      config in scope setting a rustc key or unreadable, a tracked cargo
#      config below the root, a tree git cannot list, or a rustc that
#      cannot be run or whose output carries no version
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

# The stated minimum: [workspace.package] rust-version, as written.
[[ -f Cargo.toml ]] || { echo "$me: no Cargo.toml at the repository root" >&2; exit 2; }
rust_version="$(awk '
    /^[[:space:]]*\[/ { in_pkg = ($0 ~ /^[[:space:]]*\[workspace\.package\][[:space:]]*(#.*)?$/); next }
    in_pkg && /^[[:space:]]*rust-version[[:space:]]*=/ {
        if (match($0, /"[^"]*"/)) { print substr($0, RSTART + 1, RLENGTH - 2); exit }
    }' Cargo.toml)"
[[ -n "$rust_version" ]] || { echo "$me: Cargo.toml's [workspace.package] has no rust-version = \"…\"" >&2; exit 2; }
if [[ "$rust_version" != "$channel" ]]; then
    echo "FAIL: Cargo.toml's rust-version is $rust_version; $pin_file pins $channel"
    echo "Bump them together: the stated minimum is the pinned compiler."
    exit 1
fi

# A key spelled rustc in a cargo config in scope: cargo's own config
# search, from the repository root up, then $CARGO_HOME. One pattern,
# read by the self-test too.
rustc_key_re="(^|[[:space:].{,])[\"']?rustc[\"']?[[:space:]]*="
tracked="$(git -C "$ROOT" ls-files 2>&1)" || {
    echo "$me: cannot list the tree's tracked files, so cargo configs below the root cannot be checked: $(tail -1 <<<"$tracked")" >&2
    exit 2
}
below="$(grep -E '(^|/)\.cargo/config(\.toml)?$' <<<"$tracked" | grep -Ev '^\.cargo/config(\.toml)?$' | head -1)"
if [[ -n "$below" ]]; then
    echo "$me: $below is a cargo config below the repository root, which cargo reads when run from there; this check searches from the root only" >&2
    exit 2
fi
configs=()
dir="$ROOT"
while :; do
    configs+=("$dir/.cargo/config.toml" "$dir/.cargo/config")
    [[ "$dir" == / ]] && break
    dir="$(dirname "$dir")"
done
configs+=("${CARGO_HOME:-$HOME/.cargo}/config.toml" "${CARGO_HOME:-$HOME/.cargo}/config")
for cfg in "${configs[@]}"; do
    [[ -f "$cfg" ]] || continue
    grep -Eq "$rustc_key_re" "$cfg" 2>/dev/null
    case $? in
        0) ;;
        1) continue ;;
        *) echo "$me: cannot read $cfg, a cargo config in scope" >&2; exit 2 ;;
    esac
    echo "$me: $cfg sets a key spelled rustc (build.rustc chooses cargo's compiler); this check does not resolve it — ask that compiler's --version against $pin_file by hand" >&2
    exit 2
done

rustc_bin="${RUSTC:-${CARGO_BUILD_RUSTC:-rustc}}"
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
echo "$me: rustc $version is the pinned $channel, and rust-version states it"
