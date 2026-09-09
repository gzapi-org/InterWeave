#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# >>> help
# check_vendored_advisories.sh — RustSec advisories for vendored crates
#
#   tools/checks/check_vendored_advisories.sh
#   tools/checks/check_vendored_advisories.sh --root <dir>
#
# Every crate the root manifest replaces through `[patch.crates-io]` with
# a `path` source is checked against the RustSec database at the version
# it was vendored from.
#
# WHY A BESPOKE CHECK RATHER THAN THE DEPENDENCY ONE. A path-patched
# crate has no `source` and no `checksum` in `Cargo.lock`, and
# `cargo-deny` skips it — so `check_dependencies.sh` reports clean and
# CANNOT report otherwise. This was measured rather than assumed, with
# `atty 0.2.14`, which carries RUSTSEC-2021-0145 and RUSTSEC-2024-0375:
# as an ordinary dependency `cargo deny check advisories` fails on both;
# path-patched to a copy of the same source it prints `advisories ok`.
# Vendoring therefore removes a crate from advisory coverage entirely,
# which is the opposite of what a reader expects and the reason ADR-0051
# does not simply promise that `cargo-deny` still sees it.
#
# The same shape as `check_yamux_muxer.sh`: the general tool is correct
# for the question it asks, the question just stops covering us, so the
# gap gets its own guard rather than a sentence in a document.
#
# HOW. For each patched crate, a throwaway workspace is generated in a
# temporary directory depending on that crate at that exact version FROM
# THE REGISTRY, and `cargo deny check advisories` runs there under this
# repository's own `deny.toml`, so any documented ignore still applies.
# Nothing in the working tree is touched.
#
# Dependabot cannot see a path-patched crate either, so this guard is
# also the only thing that will ever say a vendored tree needs a bump.
#
# Exit codes:
#   0  every vendored crate is free of RustSec advisories at its version
#   1  an advisory applies to a vendored crate, or a manifest is unreadable
#   2  cargo-deny is not installed, or the advisory database is
#      unreachable — a guard that passes because it could not run is the
#      shape this repository refuses
# <<< help

set -uo pipefail

ROOT="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )/../.." && pwd )"

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
    sed -n '/^# >>> help$/,/^# <<< help$/p' "$0" | sed '1d;$d;s/^# \{0,1\}//'
    exit 0
fi
if [[ "${1:-}" == "--root" ]]; then
    ROOT="${2:?--root needs a directory}"
fi
cd "$ROOT" || exit 1

die() { printf 'check_vendored_advisories: %s\n' "$1" >&2; exit "${2:-1}"; }

command -v cargo-deny >/dev/null 2>&1 || command -v cargo >/dev/null 2>&1 \
    || die "cargo is not installed" 2
cargo deny --version >/dev/null 2>&1 \
    || die "cargo-deny is not installed (CI installs a pinned build before this step)" 2

[ -f Cargo.toml ] || die "no Cargo.toml at $ROOT"
[ -f deny.toml ] || die "no deny.toml at $ROOT"

# The `[patch.crates-io]` block, one `name = { path = "..." }` per line.
# Anything without a `path` key is not a vendored tree and is not ours to
# check here.
mapfile -t patched < <(
    awk '
        /^\[patch\.crates-io\]/ { inblock = 1; next }
        /^\[/                   { inblock = 0 }
        inblock && /path[[:space:]]*=/ {
            name = $1
            match($0, /path[[:space:]]*=[[:space:]]*"[^"]+"/)
            p = substr($0, RSTART, RLENGTH)
            sub(/^path[[:space:]]*=[[:space:]]*"/, "", p)
            sub(/"$/, "", p)
            print name "\t" p
        }
    ' Cargo.toml
)

if [ "${#patched[@]}" -eq 0 ]; then
    echo "check_vendored_advisories: OK — no crate is vendored through [patch.crates-io]."
    exit 0
fi

WORK="$(mktemp -d)" || die "cannot create a temporary directory"
trap 'rm -rf "$WORK"' EXIT

violations=0
checked=0

for entry in "${patched[@]}"; do
    name="${entry%%$'\t'*}"
    path="${entry#*$'\t'}"

    [ -f "$path/Cargo.toml" ] || die "$name: no manifest at $path/Cargo.toml"
    version="$(awk '
        /^\[package\]/ { inpkg = 1; next }
        /^\[/          { inpkg = 0 }
        inpkg && /^version[[:space:]]*=/ {
            match($0, /"[^"]+"/); print substr($0, RSTART + 1, RLENGTH - 2); exit
        }
    ' "$path/Cargo.toml")"
    [ -n "$version" ] || die "$name: no version in $path/Cargo.toml"

    probe="$WORK/$name"
    mkdir -p "$probe/src"
    # The registry copy, pinned to the vendored version, so the advisory
    # lookup asks about exactly what was vendored.
    {
        printf '[package]\nname = "advisory-probe"\nversion = "0.0.0"\nedition = "2021"\n\n'
        printf '[dependencies]\n%s = "=%s"\n' "$name" "$version"
    } > "$probe/Cargo.toml"
    echo 'fn main() {}' > "$probe/src/main.rs"
    cp deny.toml "$probe/deny.toml"

    if ! (cd "$probe" && cargo generate-lockfile >/dev/null 2>&1); then
        die "$name $version: cannot resolve it from the registry" 2
    fi

    out="$(cd "$probe" && cargo deny check advisories 2>&1)"
    status=$?
    if [ "$status" -eq 0 ]; then
        checked=$((checked + 1))
        continue
    fi
    # cargo-deny exits non-zero both for a real finding and for a
    # database it could not fetch. Only the first is this guard's answer.
    if printf '%s' "$out" | grep -qiE 'unable to |failed to fetch|could not (fetch|update)|no such host'; then
        printf '%s\n' "$out" >&2
        die "$name $version: the advisory database is unreachable" 2
    fi
    printf 'check_vendored_advisories: %s %s (vendored at %s)\n' "$name" "$version" "$path" >&2
    printf '%s\n' "$out" | sed 's/^/    /' >&2
    violations=$((violations + 1))
done

if [ "$violations" -gt 0 ]; then
    printf '\ncheck_vendored_advisories: %d vendored crate(s) carry advisories.\n' "$violations" >&2
    printf 'A vendored crate is invisible to cargo-deny and to Dependabot, so this\n' >&2
    printf 'is the only warning there will be. Re-vendor a fixed release and\n' >&2
    printf 're-apply the recorded patch (ADR-0051).\n' >&2
    exit 1
fi

echo "check_vendored_advisories: OK — $checked vendored crate(s) free of RustSec advisories."
