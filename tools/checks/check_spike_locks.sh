#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# >>> help
# check_spike_locks.sh — every committed spike lock resolves AND its
# harness compiles at the revisions it pins
#
#   tools/checks/check_spike_locks.sh
#   tools/checks/check_spike_locks.sh --root <dir>
#   tools/checks/check_spike_locks.sh --no-build
#
# Two phases, per COMMITTED `Cargo.lock` under `spikes/`.
#
# RESOLVES: `cargo metadata --locked` must succeed in that directory.
# `--locked` is the whole point: it refuses to update the lock, so it
# fails when the committed lock no longer describes the build.
#
# COMPILES: `cargo check --locked` must succeed there too. THIS PHASE
# EXISTS BECAUSE THE FIRST ONE IS NOT ENOUGH, and that is measured
# rather than reasoned. `cargo metadata` resolves a dependency graph
# without type-checking a line of it, so a harness can pin a revision of
# this repository's own crates that does not contain the API its source
# imports and this guard reported OK. SPIKE-004 was in exactly that
# state: pinned at `cf04e7b7`, which defines neither `Attributing` nor
# `DialAttribution` nor `always`, while `harness/src/production.rs`
# imports all three from `interweave-transport-libp2p`. The lock
# resolved, so nothing said anything, and the harness had not been
# buildable since it adopted those symbols (found by review, PR #109;
# the pin rule was refined and the pin moved on 2026-09-20).
#
# A SPIKE THAT CANNOT BE BUILT CANNOT BE RE-RUN, which is the whole
# point of freezing it: `SPIKES.md` pins these revisions so the evidence
# can be reproduced at the versions it was measured at. A pin that no
# longer compiles preserves a graph nobody can execute.
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
# THE BUILD PHASE DRAWS THE SAME LINE, with its own sentinel. A failure
# carrying `error[E` or `could not compile` is rustc rejecting the
# source against the pinned crates -- a finding, and the one this phase
# was added for. Anything else is cargo never reaching rustc (a git
# remote it cannot fetch a pinned rev from, a registry index, a
# toolchain), which is exit 2 and not a verdict about the pin.
#
# WHY THIS DRIFTED SILENTLY, and why it no longer can. A spike harness
# is its own workspace, and it used to PATH-depend on production crates,
# which declare `libp2p = { workspace = true }` — resolving against the
# ROOT manifest wherever the crate is built, including from inside
# another workspace. So a change to the root manifest's feature list, or
# a new dependency edge on a production crate, changed what a spike's
# build needed while its committed lock stayed as it was. (Not feature
# unification, which does not cross a workspace boundary: the mechanism
# was the workspace-inherited dependency.)
#
# The 0.57 bump made that concrete — every lock broke at once, and a
# refreshed one held two libp2p majors — so the harnesses now pin this
# repository's crates BY REVISION (`SPIKES.md`, "A frozen spike is
# frozen all the way down"). That removes the drift rather than
# detecting it, and this guard becomes the check that the pinning still
# holds: a lock that stops resolving now means a rev was moved or a
# third-party requirement widened, not that the root manifest moved.
#
# IT ALSO MEANS THIS GUARD CAN NEED THE NETWORK. A revision dependency
# is fetched from the repository's own git remote, so a fetch failure
# here is not a stale lock: it produces a cargo error with no
# `--locked was passed` in it, which takes the exit-2 branch below and
# says the question could not be asked. That is the right answer, and it
# is a failure mode the path form did not have (review, PR #109).
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
# NOT CHECKED HERE: whether the spike's evidence is still valid. A lock
# that resolves and a harness that compiles say the measurement can be
# REPRODUCED, not that it still holds -- that is the spike's own record
# to make, and re-running it is a deliberate act under the regime
# `SPIKES.md` sets for that spike (frozen, or a release gate).
#
# Until 2026-09-20 this paragraph also disclaimed the build, truthfully:
# the guard did not check it. It does now, and the disclaimer went with
# the change rather than being left to contradict the code.
#
# Options:
#   --root <dir>   check this repository instead of the one containing
#                  this script
#   --no-build     the resolve phase only. For a fast local loop; CI
#                  never passes it, because the phase it skips is the
#                  one that catches what the other cannot see.
#   -h, --help     this text
#
# Exit codes:
#   0  every committed spike lock resolves under --locked AND its
#      harness compiles there (or there are none)
#   1  at least one lock is stale, or at least one harness does not
#      compile at the revisions it pins
#   2  the question could not be asked, and that is never a finding and
#      never a pass: cargo is unavailable, cargo failed for a reason
#      that is not the lock (a registry index, a manifest, a toolchain),
#      an unknown argument, `--root` without a value, or a root that
#      cannot be entered.
# <<< help

set -uo pipefail

ROOT="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )/../.." && pwd )"
BUILD=1

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
        --no-build)
            BUILD=0
            shift
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
    # `|| exit 2` rather than a bare substitution: `mapfile < <(...)`
    # discards the producer's status, so a git that failed and a git
    # that matched nothing were the same answer -- and both ended at the
    # "nothing to check" pass below. A wrong pathspec would then have
    # been a silent zero-lock OK in the one branch CI always takes
    # (review, PR #107).
    tracked="$( git ls-files -- 'spikes/*/Cargo.lock' 'spikes/**/Cargo.lock' )" || {
        echo "check_spike_locks: git could not list the tracked spike locks." >&2
        exit 2
    }
    mapfile -t locks < <(printf '%s\n' "$tracked" | grep -v '^$' | sort -u)
    # A TRACKED SPIKE LOCK EXISTS, OR THE ENUMERATION IS WRONG. Inside a
    # checkout with spike harnesses present, zero tracked locks means the
    # pathspec stopped matching rather than that the repository stopped
    # committing them -- the failure the OK line exists to prevent, and
    # invisible because it looks exactly like success.
    if [[ ${#locks[@]} -eq 0 ]] && compgen -G 'spikes/*/harness/Cargo.toml' >/dev/null; then
        echo "check_spike_locks: spike harnesses are present but git tracks no" >&2
        echo "  spikes/*/Cargo.lock. Either the pathspec here is wrong or the locks" >&2
        echo "  stopped being committed; both are findings, and neither is a pass." >&2
        exit 2
    fi
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

echo "check_spike_locks: ${#locks[@]} committed spike lock(s) resolve under --locked."

# PHASE TWO. Only reached when every lock resolved: a stale lock makes
# `cargo check --locked` fail for the reason phase one already named, so
# running it would report the same finding twice under a worse
# description.
if (( BUILD == 0 )); then
    echo "check_spike_locks: OK — resolve phase only (--no-build); the harnesses were not compiled."
    exit 0
fi

broken=0
for lock in "${locks[@]}"; do
    dir="$( dirname -- "$lock" )"
    if output="$( cd "$dir" && "${CARGO:-cargo}" check --locked --quiet 2>&1 )"; then
        echo "check_spike_locks: $dir compiles at its pinned revisions."
    elif printf '%s' "$output" | grep -q -e 'error\[E' -e 'could not compile'; then
        echo "check_spike_locks: $dir DOES NOT COMPILE at the revisions it pins." >&2
        # rustc's own first lines name the import or the type; the rest
        # is a wall this guard's output does not need.
        printf '%s\n' "$output" | grep -e 'error\[E' -e '^error' | head -3 | sed 's/^/    /' >&2
        broken=$((broken + 1))
    else
        # NOT A FINDING, by the same rule phase one uses: cargo never
        # reached rustc. A pinned rev is fetched from this repository's
        # git remote, so an offline runner fails here with nothing to
        # say about the pin.
        echo "check_spike_locks: cannot ask whether $dir compiles — cargo failed before rustc." >&2
        printf '%s\n' "$output" | head -5 | sed 's/^/    /' >&2
        exit 2
    fi
done

if (( broken > 0 )); then
    cat >&2 <<'EOF'

A harness that does not compile at its pinned revisions cannot be
re-run, which is the only thing pinning it was for. `cargo metadata`
cannot see this: it resolves the graph without type-checking it, so the
lock phase above passed while the source and the pin disagreed.

The pin is derived from the harness's LAST RECORDED RUN, not the
verdict's date (`SPIKES.md` preamble). From the harness directory:

    git rev-list -1 --first-parent --before='<last recorded run> 23:59:59' origin/main

Moving a pin is not a free edit. Under a FROZEN regime it may move only
to correct a derivation that was wrong; under a release gate only in the
change that re-runs and re-records. Read the spike's own regime first.
EOF
    exit 1
fi

echo "check_spike_locks: OK — ${#locks[@]} spike harness(es) resolve and compile at their pinned revisions."
# EXPLICIT, so the script's status is not the final `echo`'s -- it is
# right for a closed or unwritable stdout, where the echo fails without
# a signal. It does NOT rescue SIGPIPE: a review measured
# `check_spike_locks.sh | head -1` on 200k lines of output at rc 141
# both with this line and without it, because the signal kills the shell
# before `exit 0` runs. An earlier version of this comment claimed
# otherwise (review, PR #107).
exit 0
