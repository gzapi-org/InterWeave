#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/check_unused_dependencies.sh
#
# >>> help
# Every workspace member uses every dependency its manifest declares.
#
#   tools/checks/check_unused_dependencies.sh
#
# WHY THIS EXISTS. A declared dependency nobody uses still compiles,
# still sits in the graph cargo-deny judges, and still reads to a
# reviewer as "this crate reaches that one". Nothing else in the tree
# notices one: clippy judges code, not manifests. cargo-machete does.
#
# WHAT IS JUDGED: the WORKSPACE MEMBERS, read from `cargo metadata`,
# and nothing else. Not the repository root, because the spikes are
# frozen evidence (their manifests are what the spike measured) and
# third_party/ is vendored upstream code whose manifests are upstream's.
# Neither is a member, so neither is judged, and neither carries an
# ignore entry to say so. A member added anywhere, under a directory
# that does not exist today, is judged from its first commit: the list
# is the workspace's, not this file's.
#
# `--with-metadata`, NOT THE DEFAULT MODE. The default mode does not
# read [dev-dependencies] at all. MEASURED on 07b5d4bd with 0.9.2: the
# default mode reported two unused dependencies, `--with-metadata` four,
# and both extra ones were real, unused dev-dependencies.
#
# AND A CRATE CARGO-MACHETE COULD NOT READ IS NEVER A PASS. When cargo's
# metadata fails for a crate -- a registry it cannot reach, a path
# dependency that is gone -- `cargo-machete --with-metadata` prints
# "error when handling <manifest>" on stderr, says the crate is clean,
# skips it, and exits 0 (measured with 0.9.2). Two defences, because
# either alone leaves a window: the guard runs the same full
# `cargo metadata` first and refuses (exit 2) if it fails, and it reads
# cargo-machete's stderr and refuses (exit 2) on that line, which
# catches a failure between the two calls. `--locked`, as everywhere
# else in CI: a stale lockfile is exit 2 here rather than a lockfile
# cargo-machete's own metadata call would rewrite.

# A FALSE POSITIVE (a dependency used only through a macro, or only to
# pin a feature) takes an ignore entry in that crate's manifest, with
# the reason on the line above it:
#
#   [package.metadata.cargo-machete]
#   # only enables `foo/bar`, which baz needs at link time
#   ignored = ["foo"]
#
# Exit codes:
#   0  no member declares a dependency it does not use
#   1  cargo-machete named at least one — read its output above
#   2  the guard could not judge the tree: an argument was passed (it
#      takes none), cargo-machete or cargo is missing, the workspace
#      members could not be read or there are none, or cargo-machete
#      itself failed. Never a finding, and never a pass.
# <<< help

set -uo pipefail

die() { printf '%s\n' "$*" >&2; exit 2; }

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
    sed -n '/^# >>> help$/,/^# <<< help$/p' "$0" | sed '1d;$d;s/^# \{0,1\}//'
    exit 0
fi
[[ $# -eq 0 ]] || die "check_unused_dependencies: unexpected argument: $1 (the guard takes none; it judges every workspace member)"

cd "$(dirname "${BASH_SOURCE[0]}")/../.." || exit 2

if ! command -v cargo-machete >/dev/null 2>&1; then
    cat >&2 <<'MISSING'
check_unused_dependencies: cargo-machete is not installed.

  cargo install cargo-machete --locked --version 0.9.2

The version is the one CI pins (CARGO_MACHETE_VERSION in
.github/workflows/ci.yml). Exit 2 rather than 0: a guard that passes
because it could not run is the shape this repository refuses.
MISSING
    exit 2
fi
command -v cargo >/dev/null 2>&1 || die "check_unused_dependencies: cargo is not installed."
command -v jq >/dev/null 2>&1 || die "check_unused_dependencies: jq is not installed."

# The FULL metadata, not `--no-deps`: this call is the proof that
# cargo-machete's own one will succeed (see --help). The members'
# directories come from it too. Its stderr goes to a file, not into
# `listing`, where a cargo warning would corrupt the JSON.
errors=$(mktemp) || die "check_unused_dependencies: could not create a temporary file."
trap 'rm -f "$errors"' EXIT
members=()
if ! listing=$(cargo metadata --locked --format-version 1 2>"$errors"); then
    cat "$errors" >&2
    die "check_unused_dependencies: cargo metadata failed, so cargo-machete could not judge dev-dependencies (exit 2, not a pass)."
fi
root=$(jq -r '.workspace_root' <<<"$listing") || die "check_unused_dependencies: cargo metadata's output did not parse."
mapfile -t members < <(
    jq -r --arg root "$root/" '
        . as $m
        | .packages[]
        | select(.id as $id | $m.workspace_members | index($id))
        | .manifest_path
        | ltrimstr($root)
        | sub("/?Cargo\\.toml$"; "")
        | if . == "" then "." else . end' <<<"$listing" | sort -u
)
# Unreachable through cargo today -- `cargo metadata` itself refuses a
# workspace with no members -- and kept so a jq filter that stopped
# matching reads as "looked at nothing", never as a pass.
[[ ${#members[@]} -gt 0 ]] || die "check_unused_dependencies: no workspace members found — the guard would pass by looking at nothing."

# A ROOT PACKAGE IS REFUSED, not judged. Its directory is ".", and
# cargo-machete walks a path recursively, so judging it would judge
# spikes/ and third_party/ too -- the scoping this guard exists for.
# The root manifest is a virtual workspace today; a change that adds a
# root [package] has to decide how to scope it, and this says so.
# After the emptiness check: on bash before 4.4, `set -u` treats
# expanding an empty array as unbound, which would exit 1, the finding code.
for m in "${members[@]}"; do
    [[ "$m" != "." ]] || die "check_unused_dependencies: the workspace root is itself a package; cargo-machete would walk the whole tree from it, spikes/ and third_party/ included. Scope it before this guard can judge it (exit 2, not a pass)."
done

# stderr through a file, and replayed, so the "error when handling"
# line above both reaches the reader and can be read here.
machete_err=$(mktemp) || die "check_unused_dependencies: could not create a temporary file."
trap 'rm -f "$errors" "$machete_err"' EXIT
cargo-machete --with-metadata "${members[@]}" 2>"$machete_err"
rc=$?
cat "$machete_err" >&2
if grep -q '^error when handling' "$machete_err"; then
    die "check_unused_dependencies: cargo-machete could not read at least one member (above) and skipped it. Not a finding and not a pass."
fi
case $rc in
    0) ;;
    1)
        cat >&2 <<'FOUND'
check_unused_dependencies: a workspace member declares a dependency it does not use (above).

Remove it from that manifest. If it is used in a way cargo-machete cannot
see, add an ignore entry with the reason (see --help); never widen what
the guard judges to get past one.
FOUND
        exit 1
        ;;
    *)
        die "check_unused_dependencies: cargo-machete could not judge the members (its exit $rc; above). Not a finding and not a pass."
        ;;
esac

echo "check_unused_dependencies: OK — ${#members[@]} workspace members use every dependency they declare ($(cargo-machete --version 2>/dev/null | sed 's/^/cargo-machete /'))."
