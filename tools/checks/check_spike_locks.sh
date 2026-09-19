#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# >>> help
# check_spike_locks.sh — every committed spike lock still resolves
#
#   tools/checks/check_spike_locks.sh
#   tools/checks/check_spike_locks.sh --root <dir>
#
# For every `Cargo.lock` under `spikes/`, `cargo metadata --locked` must
# succeed in that directory. `--locked` is the whole point: it refuses to
# update the lock, so it fails when the committed lock no longer
# describes the build.
#
# NOT "exactly when". `cargo metadata` also fails for reasons that are
# not about the lock at all -- an unreachable registry index, a manifest
# that will not parse, a toolchain too old for the lock's version. A
# stale lock says so in cargo's own words, and that sentinel is what
# this greps for -- `--locked was passed`, the substring common to both
# phrasings cargo has used ("cannot update the lock file ... because
# --locked was passed" on the pinned 1.98, "needs to be updated but
# --locked was passed" elsewhere), measured rather than guessed; anything else exits 2 as an environment problem rather
# than reporting a finding against the diff. An earlier version called
# every failure STALE and told the author to regenerate three locks that
# were fine (review, PR #107).
#
# WHY THIS DRIFTS SILENTLY, which is the part a reader needs. A spike
# harness is its own workspace, but it path-depends on production crates,
# and those declare `libp2p = { workspace = true }` — which resolves
# against the ROOT manifest wherever the crate is built, including from
# inside another workspace. So a change to the root manifest's feature
# list, or a new dependency edge on a production crate, changes what a
# spike's build needs while its committed lock stays as it was. (Not
# feature unification, which does not cross a workspace boundary: the
# mechanism is the workspace-inherited dependency, which is why this
# guard re-resolves each spike rather than inspecting a unified set.)
#
# Nothing then complains. `cargo run` REWRITES the lock and proceeds,
# destroying the pinning a spike's README claims and with it the ability
# to reproduce the evidence the spike recorded. The failure is visible
# only to `--locked`, which nothing ran.
#
# Measured on 2026-09-19, when this guard was written: all three
# committed locks were stale, and each was missing LESS than a first
# reading of them suggested — SPIKE-002's the `interweave-discovery-api`
# package plus a `bs58` edge and an `interweave-discovery-api` edge, for
# a reason predating Stage 11 since it path-depends on no crate that
# reaches libp2p; SPIKE-003's and SPIKE-004's a single `either` edge
# each. An earlier version of this paragraph said SPIKE-003 was also
# missing `interweave-kademlia-control-api` and three edges: that had
# been resolved by intervening work, and the whole diff to that lock in
# the commit which wrote this was one line (review, PR #107 -- the
# paragraph contradicted the lockfile diffs in its own commit). Two were
# found in review; the third was found by this guard.
#
# A SPIKE WITH NO LOCK IS NOT A FAILURE. Only some harnesses commit one;
# a spike that pins nothing has nothing to drift, and this guard has
# nothing to say about it.
#
# NOT CHECKED HERE: whether the spike still builds, or whether its
# evidence is still valid. A lock that resolves says the dependency set
# is reproducible, not that the harness compiles or that its measurement
# still holds — those are the spike's own record to make.
#
# Options:
#   --root <dir>   check this repository instead of the one containing
#                  this script
#   -h, --help     this text
#
# Exit codes:
#   0  every committed spike lock resolves under --locked (or there are none)
#   1  at least one lock is stale
#   2  the question could not be asked, and that is never a finding and
#      never a pass: cargo is unavailable, cargo failed for a reason
#      that is not the lock (a registry index, a manifest, a toolchain),
#      an unknown argument, `--root` without a value, or a root that
#      cannot be entered.
# <<< help

set -uo pipefail

ROOT="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )/../.." && pwd )"

while [[ $# -gt 0 ]]; do
    case "$1" in
        -h|--help)
            sed -n '/^# >>> help$/,/^# <<< help$/p' "$0" | sed '1d;$d;s/^# \{0,1\}//'
            exit 0
            ;;
        --root)
            [[ $# -ge 2 ]] || { echo "check_spike_locks: --root needs a directory" >&2; exit 2; }
            ROOT="$2"
            shift 2
            ;;
        *)
            echo "check_spike_locks: unknown argument: $1" >&2
            exit 2
            ;;
    esac
done

cd "$ROOT" || exit 2

if ! command -v "${CARGO:-cargo}" >/dev/null 2>&1; then
    # EXIT 2, NOT 0. A guard that cannot ask its question has not
    # answered it, and a silent pass here is the failure mode the whole
    # file exists to remove.
    echo "check_spike_locks: cargo is not available; the locks could not be checked." >&2
    exit 2
fi

if [[ ! -d spikes ]]; then
    echo "check_spike_locks: no spikes/ directory; nothing to check."
    exit 0
fi

# ASKS GIT, NOT THE FILESYSTEM. This file says "committed" throughout,
# and `.gitignore` ignores `spikes/**/Cargo.lock` while re-admitting
# exactly three. A developer who runs SPIKE-006's harness produces an
# ignored lock, and a `find` counted it -- so the OK line reported a
# number of files that are not committed, in a sentence whose whole job
# is to stop a zero-lock pass reading as a real one. Four sibling guards
# ask git for the same reason (review, PR #107).
if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    mapfile -t locks < <(git ls-files -- 'spikes/**/Cargo.lock' | sort)
else
    # `--root` may point at a tree that is not a checkout; there is
    # nothing to ask git about, so the filesystem is the only answer.
    mapfile -t locks < <(find spikes -name Cargo.lock -type f 2>/dev/null | sort)
fi
if [[ ${#locks[@]} -eq 0 ]]; then
    echo "check_spike_locks: no committed spike locks; nothing to check."
    exit 0
fi

stale=0
for lock in "${locks[@]}"; do
    dir="$( dirname -- "$lock" )"
    if output="$( cd "$dir" && "${CARGO:-cargo}" metadata --locked --format-version 1 2>&1 >/dev/null )"; then
        echo "check_spike_locks: $lock resolves."
    elif printf '%s' "$output" | grep -q -- '--locked was passed'; then
        echo "check_spike_locks: $lock is STALE — cargo metadata --locked fails." >&2
        # The first line of cargo's complaint names what is missing or
        # what would have to change; the rest is a backtrace nobody needs
        # in a check's output.
        printf '%s\n' "$output" | head -3 | sed 's/^/    /' >&2
        stale=$((stale + 1))
    else
        # NOT A FINDING. cargo failed for a reason that is not the lock:
        # the registry index, a manifest, the toolchain. Reporting that
        # as STALE reds a required context with a diagnosis pointing at
        # the diff, and sends the author to regenerate a lock that is
        # fine. `check_vendored_advisories.sh` draws the same line.
        echo "check_spike_locks: cannot ask about $lock — cargo failed for another reason." >&2
        printf '%s\n' "$output" | head -5 | sed 's/^/    /' >&2
        exit 2
    fi
done

if (( stale > 0 )); then
    cat >&2 <<'EOF'

A committed spike lock that no longer resolves is a spike whose evidence
cannot be reproduced at the versions it recorded: the next `cargo run`
rewrites the lock, silently, and the pinning the README claims is gone.

Update each stale lock MINIMALLY, from its harness directory:

    cd <harness> && cargo metadata --format-version 1 >/dev/null

That resolves what is missing and moves nothing else. Do NOT run
`cargo generate-lockfile` here: it rewrites the lock from scratch, and
when it was tried on these three it moved eighty-odd packages onto newer
patch versions -- which destroys the pinning this guard exists to
protect, so following that advice would undo what the failure is telling
you about.

Commit the result with the change that moved the root manifest, so the
two travel together. A spike whose lock is refreshed for a reason
unrelated to its own evidence is worth a line in its README.
EOF
    exit 1
fi

echo "check_spike_locks: OK — ${#locks[@]} committed spike lock(s) resolve under --locked."
# EXPLICIT, because a bare final `echo` makes the script's status that
# of the echo: `check_spike_locks.sh | head -1` then exits non-zero on
# EPIPE and reads as a stale lock (review, PR #107).
exit 0
