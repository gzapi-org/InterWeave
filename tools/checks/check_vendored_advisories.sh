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
# Every crate this workspace builds from a tree inside the repository
# rather than from the registry -- a `[patch.crates-io]` path entry, or a
# plain path dependency that is not a workspace member -- is checked
# against the RustSec database at the version it was vendored from.
# Advisories on that crate's own dependencies are NOT this guard's; they
# belong to `check_dependencies.sh`, which judges the committed lockfile.
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
# HOW. The vendored set comes from `cargo metadata`, not from reading
# Cargo.toml: a patched crate is exactly a package cargo resolved with no
# `source` that is not a workspace member. An earlier version parsed the
# `[patch.crates-io]` table with awk and was blind to the equally valid
# `[patch.crates-io.<crate>]` sub-table form -- it found nothing, said so,
# and exited 0, which is a false pass in a guard whose whole job is to be
# the only warning there is. Asking cargo removes that family of bug
# whole: sub-tables, comments, `package = ` renames, quoted keys and CRLF
# are all cargo's problem and it has already solved them.
#
# For each such crate a throwaway workspace is generated in a temporary
# directory depending on it at that exact version FROM THE REGISTRY, and
# `cargo deny check advisories` runs there under this repository's own
# `deny.toml`, so any documented ignore still applies. Nothing in the
# working tree is touched.
#
# Dependabot cannot see a path-patched crate either, so this guard is
# also the only thing that will ever say a vendored tree needs a bump.
#
# TWO THINGS IT STILL CANNOT SEE, both recorded rather than papered over.
# An `ignore` entry in `deny.toml` silences an advisory here as well, and
# one added because a REGISTRY dependency carries it also removes the
# vendored crate's only coverage -- so an ignore touching a crate that is
# also vendored wants a note saying so. And this asks about the version
# the vendored manifest DECLARES, not about the bytes: a tree whose
# version string does not match the release it was taken from redirects
# the question to a different release. Tying the bytes to the tarball is
# ADR-0051's named follow-up.
#
# Exit codes:
#   0  every vendored crate is free of RustSec advisories at its version
#   1  an advisory applies to a vendored crate
#   2  the check could not be RUN: cargo-deny absent, the lockfile
#      unusable, or cargo-deny stopping before it produced a summary — an
#      unreachable database, a config it cannot read, a crash. Success is
#      proved by that summary and never inferred from an absence of
#      findings, because a guard that passes because it could not run is
#      the shape this repository refuses — and this script has done it
#      three times.
# <<< help

set -uo pipefail

ROOT="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )/../.." && pwd )"

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
    sed -n '/^# >>> help$/,/^# <<< help$/p' "$0" | sed '1d;$d;s/^# \{0,1\}//'
    exit 0
fi
if [[ "${1:-}" == "--root" ]]; then
    ROOT="${2:?--root needs a directory}"
elif [ -n "${1:-}" ]; then
    printf 'check_vendored_advisories: unknown argument %s\n' "$1" >&2
    exit 1
fi
cd "$ROOT" || exit 1

die() { printf 'check_vendored_advisories: %s\n' "$1" >&2; exit "${2:-1}"; }

command -v cargo-deny >/dev/null 2>&1 || command -v cargo >/dev/null 2>&1 \
    || die "cargo is not installed" 2
cargo deny --version >/dev/null 2>&1 \
    || die "cargo-deny is not installed (CI installs a pinned build before this step)" 2

[ -f Cargo.toml ] || die "no Cargo.toml at $ROOT"
[ -f deny.toml ] || die "no deny.toml at $ROOT"
command -v python3 >/dev/null 2>&1 || die "python3 is not installed" 2

# `--locked` WITH NO FALLBACK. Re-resolving would rewrite the
# repository's `Cargo.lock`, which this guard promises not to do -- and a
# lockfile that does not satisfy the manifest is a real problem, not
# something to route around. Review finding on PR #85.
# `--all-features`, for the reason `deny.toml` gives for its own
# `all-features = true`: a dependency that appears only under a
# non-default feature is still governed. Without it, vendoring a crate
# behind an optional or gated feature left it out of the graph entirely
# and this guard announced that nothing was vendored -- and CLAUDE.md §1
# records that the connectivity behaviours ship GATED OFF, so that is one
# manifest edit away. Review finding on PR #85.
metadata="$(cargo metadata --format-version 1 --locked --all-features 2>/dev/null)" \
    || die "cargo metadata --locked failed at $ROOT; is Cargo.lock current?" 2

# A package cargo resolved with no `source` and which is not a workspace
# member is a vendored crate: `[patch.crates-io]` with a path, or a plain
# path dependency outside the workspace. Both are trees this repository
# ships and neither is covered by the advisory check.
# CAPTURED FIRST, THEN SPLIT. `mapfile -t x < <(cmd)` exits 0 whatever
# `cmd` did, so `mapfile ... || die` is dead code: a python that failed
# left an empty array and this guard announced that nothing is vendored.
# That is the same false pass the awk parser gave, one layer down, in the
# guard whose exit table refuses exactly it. Review finding on PR #85.
selected="$(printf '%s' "$metadata" | python3 -c '
import json, os, sys
meta = json.load(sys.stdin)
members = set(meta.get("workspace_members", []))
root = os.path.realpath(meta.get("workspace_root", "."))
for pkg in meta.get("packages", []):
    if pkg.get("source") is not None or pkg["id"] in members:
        continue
    manifest = os.path.realpath(pkg["manifest_path"])
    if not manifest.startswith(root + os.sep):
        continue
    print("\t".join((pkg["name"], pkg["version"], os.path.dirname(manifest))))
')" || die "cannot read the package graph" 2

patched=()
while IFS= read -r line; do
    [ -n "$line" ] && patched+=("$line")
done <<< "$selected"

# THE FLOOR. "The graph named nothing" and "nothing is vendored" are
# different claims, and conflating them is how this guard reported
# success twice before. A directory under a vendored root with its own
# manifest is a tree this repository ships, so if one exists and the
# graph did not account for it, something is wrong with the question --
# the crate is behind a disabled feature, or listed as a workspace
# member, or reached through a symlink, or patched but unused. Every one
# of those produced a confident "nothing is vendored" and exit 0.
# Review findings on PR #85.
shipped=0
for manifest in third_party/*/Cargo.toml; do
    [ -f "$manifest" ] && shipped=$((shipped + 1))
done

if [ "${#patched[@]}" -eq 0 ]; then
    if [ "$shipped" -gt 0 ]; then
        die "third_party/ holds $shipped vendored crate(s) but the package graph names none -- a disabled feature, a workspace member, a symlink, or an unused patch would each do this, and none of them is a reason to report success" 2
    fi
    echo "check_vendored_advisories: OK — nothing is vendored."
    exit 0
fi

if [ "${#patched[@]}" -lt "$shipped" ]; then
    die "third_party/ holds $shipped vendored crate(s) and the package graph names only ${#patched[@]}" 2
fi

WORK="$(mktemp -d)" || die "cannot create a temporary directory"
trap 'rm -rf "$WORK"' EXIT

violations=0
checked=0

for entry in "${patched[@]}"; do
    IFS=$'\t' read -r name version path <<< "$entry"
    # The row protocol is tab-separated lines, so a path carrying either
    # would be split wrong and checked as a different crate.
    case "$path" in
        *"$(printf '\t')"*) die "$name: vendored path contains a tab" 2 ;;
    esac
    [ -d "$path" ] || die "$name $version: vendored path $path is not a directory" 2

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

    out="$(cd "$probe" && cargo deny --format json check advisories 2>&1)"

    # SUCCESS IS PROVED, NEVER INFERRED FROM ABSENCE.
    #
    # And the unreachable-database message is reached only when the run
    # did NOT complete. An earlier version grepped network wording over
    # the whole merged output, which includes every advisory's own
    # description -- so a real finding whose text happens to contain
    # "unable to" was reported as an unreachable database, exit 2, which
    # tells an operator to re-run rather than to act. `rand 0.9.0`
    # (RUSTSEC-2026-0097) is one such advisory and `rand` is in this
    # workspace. Review finding on PR #85. cargo-deny ends a
    # completed run with a `{"type":"summary"}` line carrying its
    # advisory counts; anything that stops it earlier -- a config it
    # cannot deserialize, a corrupt database, a crash, an output-schema
    # change -- produces no such line. An earlier version classified
    # FAILURES instead, by matching network wording, and so let every
    # unclassified failure fall through to a filter that found no
    # advisory and called the crate clean. That is the third time this
    # script has reported success without having checked, so it now
    # requires the positive signal rather than trying to enumerate the
    # ways of not getting one. Confirmed by construction with an
    # undeserializable `deny.toml`. Review findings on PR #85.
    #
    # The filter reports ONLY advisories naming the vendored crate. The
    # probe pins that crate and lets cargo resolve its dependencies
    # fresh, so the graph it judges is not one any commit here contains;
    # a future advisory on a transitive dependency at latest would turn a
    # required check red and name the wrong crate. Those belong to
    # `check_dependencies.sh`, which reads the committed lockfile.
    findings="$(printf '%s' "$out" | python3 -c '
import json, sys

name = sys.argv[1]
completed = False
found = []
for line in sys.stdin:
    line = line.strip()
    if not line.startswith("{"):
        continue
    try:
        record = json.loads(line)
    except ValueError:
        continue
    field = record.get("fields", {})
    if record.get("type") == "summary" and "advisories" in field:
        completed = True
        continue
    # ERROR **OR** WARNING. For a registry crate a warning still reaches
    # a human through `check_dependencies.sh` output; for a vendored one
    # this guard is the only report there is, so a policy that downgrades
    # advisories must not silence it here.
    if field.get("severity") not in ("error", "warning"):
        continue
    if not any(g.get("Krate", {}).get("name") == name for g in field.get("graphs", [])):
        continue
    advisory = field.get("advisory", {})
    if not advisory:
        continue
    found.append("    {} [{}]: {}".format(advisory.get("id", field.get("code", "?")),
                                          field.get("severity", "?"),
                                          advisory.get("title", field.get("message", ""))))
if not completed:
    sys.exit(3)
sys.stdout.write("".join(f + "\n" for f in found))
' "$name")"
    case $? in
        0) ;;
        3)
            # No summary: the run did not complete. Say which kind, from
            # cargo-deny's own log records rather than from advisory text.
            if printf '%s' "$out" \
                | grep -E '"(type|level)":"(log|ERROR)"' \
                | grep -qiE 'unable to |failed to fetch|could not (fetch|update)|no such host'; then
                die "$name $version: the advisory database is unreachable" 2
            fi
            printf '%s\n' "$out" >&2
            die "$name $version: cargo-deny did not complete a run — its output carries no summary, so nothing was checked" 2
            ;;
        *) die "$name $version: cannot read the advisory report" 2 ;;
    esac

    if [ -z "$findings" ]; then
        checked=$((checked + 1))
        continue
    fi
    printf 'check_vendored_advisories: %s %s (vendored at %s)\n' "$name" "$version" "$path" >&2
    printf '%s\n' "$findings" >&2
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
