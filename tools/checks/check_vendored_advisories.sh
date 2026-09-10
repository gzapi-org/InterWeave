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
# Every crate this workspace builds from a local tree rather than the
# registry is checked against the RustSec database at the version it
# declares. Three sources say what that is, because each sees what the
# others miss: the package graph (what cargo builds from a local tree,
# wherever it lives), every manifest under `third_party/` at any depth
# (what this repository ships), and every `path` a `[patch.*]` table
# declares in the manifest OR in `.cargo/config.toml` (which catches a
# patch cargo omitted from the graph for being unused, wherever it was
# declared). They must account for each other -- a shipped tree the graph
# does not name, or a local crate with no registry release, is exit 2
# rather than a pass. Advisories on a
# vendored crate's own dependencies are NOT this guard's; they belong to
# `check_dependencies.sh`, which judges the committed lockfile.
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
# HOW. See the union below; `cargo metadata` supplies each crate's
# version, so a rename or a workspace inheritance is cargo's problem
# rather than this script's.
#
# Every shape here was learned from a false pass. An early version parsed
# the `[patch.crates-io]` table with awk and was blind to the equally
# valid `[patch.crates-io.<crate>]` sub-table form -- it found nothing,
# said so, and exited 0. Asking cargo removed that family whole. Scoping
# the question to what cargo RESOLVED then missed a tree cargo had not
# resolved -- behind a disabled feature, patched but unused -- and
# scoping it to DISK instead missed a patch path outside `third_party/`
# and a tree nested deeper than one level, one shape of which printed an
# affirmative count while the vulnerable tree went unasked. Comparing
# COUNTS rather than identities let one tree stand in for another.
#
# The lesson, recorded because it took six rounds: neither source of
# truth is sufficient, and the attractive move each time was to swap one
# for the other rather than to make them check each other.
#
# For each tree a throwaway workspace is generated in a temporary
# directory depending on it at that exact version FROM THE REGISTRY, and
# `cargo deny check advisories` runs there under this repository's own
# `deny.toml`, so any documented ignore still applies. Nothing in the
# working tree is touched.
#
# Dependabot cannot see a path-patched crate either, so this guard is
# also the only thing that will ever say a vendored tree needs a bump.
#
# A tree with no registry release cannot be asked about at all, and that
# is reported as an incomplete sweep (exit 2) rather than as a pass --
# `deny.toml` bars git dependencies, so vendoring is the sanctioned route
# for such a crate and the case is reachable by design.
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

# DEFINED BEFORE ITS FIRST USE. `cd "$ROOT" || die` ran while `die` was
# still undefined, so bash printed `die: command not found` and -- with no
# `set -e` -- CARRIED ON in the caller's directory, which could report a
# verdict for the wrong repository. Review finding on PR #85.
die() { printf 'check_vendored_advisories: %s\n' "$1" >&2; exit "${2:-1}"; }

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
cd "$ROOT" || die "cannot enter $ROOT" 2


command -v cargo-deny >/dev/null 2>&1 || command -v cargo >/dev/null 2>&1 \
    || die "cargo is not installed" 2
cargo deny --version >/dev/null 2>&1 \
    || die "cargo-deny is not installed (CI installs a pinned build before this step)" 2

[ -f Cargo.toml ] || die "no Cargo.toml at $ROOT" 2
[ -f deny.toml ] || die "no deny.toml at $ROOT" 2
command -v python3 >/dev/null 2>&1 || die "python3 is not installed" 2

# `--all-features`, for the reason `deny.toml` gives for its own
# `all-features = true`: a dependency that appears only under a
# non-default feature is still governed. Without it, vendoring a crate
# behind an optional or gated feature left it out of the graph entirely
# and this guard announced that nothing was vendored -- and CLAUDE.md §1
# records that the connectivity behaviours ship GATED OFF, so that is one
# manifest edit away. Review finding on PR #85.
metadata="$(cargo metadata --format-version 1 --locked --all-features 2>/dev/null)" \
    || die "cargo metadata --locked failed at $ROOT; is Cargo.lock current?" 2

# TWO SOURCES OF TRUTH, EACH CHECKING THE OTHER. Neither alone is
# enough, and choosing between them has now failed twice in opposite
# directions.
#
# THE GRAPH says what cargo actually builds from a local tree: a package
# with no `source`, under the workspace root, that is either not a
# workspace member or lives under a vendored root. That is authoritative
# and it catches a `[patch.crates-io]` path anywhere, not only under
# `third_party/` -- which ADR-0051 Decision 7 promises. Scoping to disk
# alone missed exactly that, and missed a tree nested deeper than one
# level, and in one shape printed an affirmative count while the
# vulnerable tree went unasked.
#
# THE DISK says what this repository SHIPS: every manifest under a
# vendored root, at any depth. That is what catches a tree cargo did not
# resolve -- behind a disabled feature, patched but unused, or reached
# some way the graph does not name -- each of which a graph-only scan
# reported as "nothing is vendored", exit 0.
#
# So every disk manifest must appear in the graph selection, and every
# graph selection is checked. A mismatch in either direction is exit 2.
# Review findings on PR #85.
# WHAT THIS REPOSITORY SHIPS, from two places that each see what the
# other misses: every manifest under `third_party/`, and every `path` a
# `[patch.*]` table declares. The patch table is needed because cargo
# omits an UNUSED patch from the graph entirely -- so a patch pointing
# outside `third_party/` that nothing currently depends on was in neither
# the graph nor the disk scan, and the guard announced that nothing is
# built from a local tree. The tree still ships, and a dependency bump
# re-arms it. Read with `tomllib` rather than matched, because hand-parsing
# this table is what the first version of this guard got wrong.
# Review finding on PR #85.
mapfile -t shipped < <(
    find third_party -name Cargo.toml -type f 2>/dev/null
    python3 - Cargo.toml .cargo/config.toml .cargo/config <<'PATCHPATHS'
import os, sys, tomllib

# A `[patch]` table lives in the manifest OR in Cargo's own configuration
# -- the Cargo reference says so in as many words -- and cargo omits an
# unused patch from the graph wherever it was declared. Reading only the
# manifest left a config-declared patch invisible, which a reviewer
# constructed. Relative paths in both resolve against the directory
# holding the file's parent, which is this working directory for all
# three, since the caller has already entered the root.
for source in sys.argv[1:]:
    try:
        with open(source, "rb") as handle:
            document = tomllib.load(handle)
    except FileNotFoundError:
        continue
    except (OSError, tomllib.TOMLDecodeError) as exc:
        sys.stderr.write("cannot read {}: {}\n".format(source, exc))
        raise SystemExit(5)
    for table in document.get("patch", {}).values():
        if not isinstance(table, dict):
            continue
        for entry in table.values():
            path = entry.get("path") if isinstance(entry, dict) else None
            if path:
                print(os.path.join(os.path.realpath(path), "Cargo.toml"))
PATCHPATHS
)
# FILTERED AND DEDUPLICATED. `printf '%s\n' "${arr[@]}"` on an EMPTY
# array prints one blank line, which `sort -u` keeps -- and a blank
# "manifest" has the workspace root as its directory, so a repository
# vendoring nothing reported the root itself as an unaccounted tree.
mapfile -t shipped < <(
    for manifest in ${shipped[@]+"${shipped[@]}"}; do
        [ -n "$manifest" ] && printf '%s\n' "$manifest"
    done | sort -u
)

selected="$(printf '%s' "$metadata" | python3 -c '
import json, os, sys

shipped = {os.path.realpath(os.path.dirname(p)) for p in sys.argv[1:]}
meta = json.load(sys.stdin)
members = set(meta.get("workspace_members", []))
root = os.path.realpath(meta.get("workspace_root", "."))
vendored_root = os.path.join(root, "third_party")

rows = {}
for pkg in meta.get("packages", []):
    if pkg.get("source") is not None:
        continue
    directory = os.path.realpath(os.path.dirname(pkg["manifest_path"]))
    if directory != root and not directory.startswith(root + os.sep):
        continue
    under_vendored = directory == vendored_root or directory.startswith(vendored_root + os.sep)
    # A first-party crate is a member and is not vendored. A vendored one
    # is checked whether or not someone listed it as a member.
    if pkg["id"] in members and not under_vendored:
        continue
    rows[directory] = "\t".join((pkg["name"], pkg["version"], directory))

unaccounted = sorted(shipped - rows.keys())
if unaccounted:
    sys.stderr.write("".join("    not in the package graph: " + u + "\n" for u in unaccounted))
    sys.exit(4)
sys.stdout.write("".join(r + "\n" for r in (rows[k] for k in sorted(rows))))
' "${shipped[@]}")"
case $? in
    0) ;;
    4)
        die "a vendored tree this repository ships is absent from the package graph — behind a disabled feature, patched but unused, or a manifest that is a workspace root rather than a package. None of those is a reason to report success" 2
        ;;
    *) die "cannot read the package graph" 2 ;;
esac

patched=()
while IFS= read -r line; do
    [ -n "$line" ] && patched+=("$line")
done <<< "$selected"

if [ "${#patched[@]}" -eq 0 ]; then
    echo "check_vendored_advisories: OK — nothing is built from a local tree."
    exit 0
fi

WORK="$(mktemp -d)" || die "cannot create a temporary directory" 2
trap 'rm -rf "$WORK"' EXIT

violations=0
checked=0
unaskable=()

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

    # NO REGISTRY RELEASE IS ITS OWN ANSWER, not a reason to abandon the
    # sweep. `deny.toml` bars git dependencies, so vendoring is the
    # sanctioned route for a crate that was never published -- and such a
    # crate cannot be asked about. An earlier version died here on the
    # first one, so a later vendored crate carrying a real advisory was
    # never reached, under a message that reads like a broken runner.
    # Review findings on PR #85.
    if ! (cd "$probe" && cargo generate-lockfile >/dev/null 2>&1); then
        unaskable+=("$name $version ($path)")
        continue
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

if [ "${#unaskable[@]}" -gt 0 ]; then
    printf '\ncheck_vendored_advisories: %d tree(s) have no registry release, so their\n' \
        "${#unaskable[@]}" >&2
    printf 'advisories cannot be asked about:\n' >&2
    printf '    %s\n' "${unaskable[@]}" >&2
fi

if [ "$violations" -gt 0 ]; then
    printf '\ncheck_vendored_advisories: %d vendored crate(s) carry advisories.\n' "$violations" >&2
    printf 'A vendored crate is invisible to cargo-deny and to Dependabot, so this\n' >&2
    printf 'is the only warning there will be. Re-vendor a fixed release and\n' >&2
    printf 're-apply the recorded patch (ADR-0051).\n' >&2
    exit 1
fi

if [ "${#unaskable[@]}" -gt 0 ]; then
    printf '\ncheck_vendored_advisories: the checked trees are clean, but the sweep\n' >&2
    printf 'was not complete. Exit 2 rather than 0, because reporting success for\n' >&2
    printf 'a set this guard could not ask about is the shape it exists to refuse.\n' >&2
    exit 2
fi

echo "check_vendored_advisories: OK — $checked vendored crate(s) free of RustSec advisories."
