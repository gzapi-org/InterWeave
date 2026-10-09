#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# tools/ci/build_previous_builds.sh
#
# >>> help
# Build every previous production build the upgrade matrix runs against.
#
#   tools/ci/build_previous_builds.sh [--list <file>] <dir>
#
# testing.md §Compatibility fixtures: the matrix's "previous build" axis
# is production builds, each built from its own commit as a second
# binary. The entries are tools/ci/previous-builds.txt ("<label> <sha>"
# per line, `#` comments and blank lines skipped; --list names another
# file). For each entry this leaves
#
#   <dir>/<label>/transport-daemon
#   <dir>/<label>/transportctl
#
# built in release with --locked from that commit's own tree and
# Cargo.lock, in a detached worktree and a target directory of their
# own, so nothing of the current checkout or its target leaks in. Each
# app is built by --manifest-path (apps/transport-daemon,
# apps/transportctl), so a package renamed since does not matter; the
# binary names are the interface. The `rust` job exports <dir> as
# INTERWEAVE_PREVIOUS_BUILDS to
# tests/interoperability/tests/upgrade_matrix_previous.rs.
#
# An entry whose two binaries are already there and executable is not
# rebuilt: CI restores <dir> from a cache keyed on the list and this
# script, and a commit's build never changes. That is safe because a
# label must end in its sha's first eight characters, so one label can
# never name two commits. An incomplete entry is removed and built
# again; an entry is moved into place only once both binaries exist,
# so an interrupted run leaves none half-built. The commit is fetched
# (depth 1) only when the clone lacks it, as CI's shallow checkout does.
# The worktree and target directory are removed however the run ends.
# Subdirectories of <dir> not on the list are left alone; the rows are
# what refuse them.
#
# The list is checked whole before anything is built: a label is
# [a-z0-9][a-z0-9.-]* and unique, a sha is 40 lowercase hex, and the
# label ends in "-<first 8 of the sha>". An empty list fails: the
# matrix has an entry, and a list that lost it is a mistake.
#
# Needs: git, cargo (rustup, when present, installs the toolchain each
# commit's rust-toolchain.toml pins, as the `rust` job's first step
# does for the current one).
#
# Exit codes:
#   0  every entry is in <dir>
#   1  the list is malformed, or a fetch, worktree or build failed
#   2  usage
# <<< help

set -euo pipefail

SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
ROOT="$( cd -- "$SCRIPT_DIR/../.." && pwd )"
LIST="$SCRIPT_DIR/previous-builds.txt"
APPS=(transport-daemon transportctl)

usage() { sed -n '/^# >>> help$/,/^# <<< help$/{/^# >>> help$/d;/^# <<< help$/d;s/^# \{0,1\}//;p}' "${BASH_SOURCE[0]}"; }
die() { echo "build_previous_builds: $*" >&2; exit 1; }

dir=""
while [ $# -gt 0 ]; do
    case "$1" in
        -h|--help) usage; exit 0 ;;
        --list) [ $# -ge 2 ] || { usage >&2; exit 2; }; LIST="$2"; shift 2 ;;
        -*) echo "build_previous_builds: unknown option $1" >&2; usage >&2; exit 2 ;;
        *) [ -z "$dir" ] || { usage >&2; exit 2; }; dir="$1"; shift ;;
    esac
done
[ -n "$dir" ] || { usage >&2; exit 2; }
[ -f "$LIST" ] || die "$LIST: no such list"

# The whole list, checked before the first build.
labels=(); shas=()
n=0
while IFS= read -r line || [ -n "$line" ]; do
    n=$((n + 1))
    line="${line%%#*}"
    read -r label sha extra <<<"$line" || true
    [ -n "${label:-}" ] || continue
    [ -z "${extra:-}" ] || die "$LIST:$n: expected \"<label> <sha>\", got more"
    [[ "$label" =~ ^[a-z0-9][a-z0-9.-]*$ ]] || die "$LIST:$n: label '$label' is not [a-z0-9][a-z0-9.-]*"
    [[ "${sha:-}" =~ ^[0-9a-f]{40}$ ]] || die "$LIST:$n: '${sha:-}' is not a full 40-hex commit"
    [[ "$label" == *"-${sha:0:8}" ]] || die "$LIST:$n: label '$label' does not end in -${sha:0:8}, its commit's prefix"
    for seen in "${labels[@]}"; do
        [ "$seen" != "$label" ] || die "$LIST:$n: label '$label' is listed twice"
    done
    labels+=("$label"); shas+=("$sha")
done <"$LIST"
[ "${#labels[@]}" -gt 0 ] || die "$LIST lists no entry"

mkdir -p -- "$dir"
dir="$( cd -- "$dir" && pwd )"

complete() {
    local app
    for app in "${APPS[@]}"; do [ -x "$1/$app" ] && [ -f "$1/$app" ] || return 1; done
}

scratch=""
src=""
partial=""
cleanup() {
    [ -z "$partial" ] || rm -rf -- "$partial"
    [ -z "$src" ] || git -C "$ROOT" worktree remove --force "$src" >/dev/null 2>&1 || true
    [ -z "$scratch" ] || rm -rf -- "$scratch"
    # After the rm: prune drops only a registration whose directory is
    # gone, so it is the backstop for a remove that failed.
    [ -z "$src" ] || git -C "$ROOT" worktree prune >/dev/null 2>&1 || true
}
trap cleanup EXIT

for i in "${!labels[@]}"; do
    label="${labels[$i]}"; sha="${shas[$i]}"
    out="$dir/$label"
    if complete "$out"; then
        echo "build_previous_builds: $label ($sha) is already in $dir"
        continue
    fi
    rm -rf -- "$out" "$out.partial"
    echo "build_previous_builds: building $label from $sha"

    if ! git -C "$ROOT" cat-file -e "$sha^{commit}" 2>/dev/null; then
        git -C "$ROOT" fetch --quiet --no-tags --depth 1 origin "$sha" \
            || die "$label: could not fetch $sha from origin"
    fi
    scratch="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/previous-build.XXXXXX")"
    src="$scratch/src"
    git -C "$ROOT" worktree add --quiet --detach "$src" "$sha" \
        || die "$label: could not check $sha out into a worktree"
    if command -v rustup >/dev/null 2>&1; then
        ( cd "$src" && { rustup show active-toolchain >/dev/null 2>&1 || rustup toolchain install; } ) \
            || die "$label: could not install the toolchain $sha pins"
    fi
    partial="$out.partial"
    mkdir -p -- "$partial"
    for app in "${APPS[@]}"; do
        ( cd "$src" && CARGO_TARGET_DIR="$scratch/target" \
            cargo build --release --locked --manifest-path "apps/$app/Cargo.toml" --bin "$app" ) \
            || die "$label: cargo could not build $app at $sha"
        [ -f "$scratch/target/release/$app" ] && [ -x "$scratch/target/release/$app" ] \
            || die "$label: cargo succeeded and left no executable target/release/$app"
        cp -- "$scratch/target/release/$app" "$partial/$app"
    done
    mv -- "$partial" "$out"
    partial=""
    cleanup
    scratch=""; src=""
    echo "build_previous_builds: $label is in $out"
done
