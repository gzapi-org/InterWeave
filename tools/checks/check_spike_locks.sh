#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# >>> help
# check_spike_locks.sh — every committed spike lock resolves, its pin
# can be accounted for, and its harness compiles at that pin
#
#   tools/checks/check_spike_locks.sh
#   tools/checks/check_spike_locks.sh --root <dir>
#   tools/checks/check_spike_locks.sh --no-build
#   tools/checks/check_spike_locks.sh --no-provenance
#
# Three phases, per COMMITTED `Cargo.lock` under `spikes/`.
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
# THE BUILD PHASE DRAWS THE SAME LINE, in three steps rather than one.
# `error[E` is rustc's own coded diagnostic and settles it: a finding.
# Failing that, a `(signal:` with no other `error:` line means the
# compiler was killed rather than answering -- exit 2, because a runner
# that ran out of memory must not be reported as a bad pin. Failing
# both, a `could not compile` is a finding: plenty of real rustc errors
# carry no `[Ennnn]`. Anything else is cargo never reaching rustc (a git
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
#                  this script. An exported tree with no `.git` needs
#                  `--no-provenance` too, since the provenance phase
#                  refuses rather than skips.
#   --no-build     skip the compile phase. For a fast local loop; CI
#                  never passes it, because the phase it skips is the
#                  one that catches what the others cannot see.
#   --no-provenance
#                  skip the pin-provenance phase. For the states it
#                  refuses: a tree that is not a checkout, one with no
#                  `origin/main`, a shallow clone, and a pin whose
#                  commit this clone does not have.
#   -h, --help     this text
#
# Exit codes:
#   0  every committed spike lock resolves under --locked, every pin is
#      accounted for, and every harness compiles there (or there are
#      none)
#   1  at least one lock is stale, at least one pin cannot be accounted
#      for, or at least one harness does not compile at the revisions it
#      pins
#   2  the question could not be asked, and that is never a finding and
#      never a pass: cargo is unavailable, cargo failed for a reason
#      that is not the lock (a registry index, a manifest, a toolchain),
#      an unknown argument, `--root` without a value, a root that cannot
#      be entered, a compiler killed by a signal rather than answering,
#      or -- with the provenance phase on -- a tree that is not a git
#      checkout, a checkout with no `origin/main`, a shallow clone, or a
#      pin whose commit this clone does not have, none of which can say
#      where a pin came from.
# <<< help

set -uo pipefail

ROOT="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )/../.." && pwd )"
BUILD=1
PROVENANCE=1

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
        --no-provenance)
            PROVENANCE=0
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

# PHASE TWO: WHERE THE PIN CAME FROM. Four mechanical questions, all
# cheap, and none of them the proof.
#
#   1. The pin is an ancestor of `origin/main`. A pin that is not is a
#      feature-branch tip that never merged, which is what the FIRST
#      version of these pins recorded (review, PR #109).
#   2. The pin is one of the two trees a run-recording commit in that
#      spike's history points at: its PARENT when the commit changes no
#      production crate, or the commit ITSELF when it does.
#   3. Every dependency on this repository carries a revision this can
#      read -- decided per dependency, so an unpinned one cannot hide
#      behind a pinned sibling.
#   4. One harness pins one tree. Several dependencies at different
#      commits is a harness reproducing nothing in particular. `SPIKES.md`'s rule: the pin is the tree the last
#      recorded run built against, and a recording commit touches only
#      the spike, so the production crates it resolved by path are its
#      parent's. A date-derived pin fails this, which is how the second
#      wrong pin was caught (review, PR #110).
#
# NEITHER IS THE PROOF, and this must not be read as one. The proof that
# a pin is the right tree is a reproduction run matching the recorded
# observations, made by hand when a pin is set or questioned and cited
# beside it. These two are what a wrong pin has failed BOTH times, which
# is worth a guard; they would pass for a pin that compiles and measures
# the wrong thing.
#
# NEEDS HISTORY. A pinned revision is an ordinary commit object, so a
# shallow clone cannot answer either question. That is exit 2 and says
# which knob fixes it, rather than a pass -- the whole file's rule.
# THE MANIFEST READER the provenance phase uses. Kept here rather than
# inline so the phase below reads as the questions it asks, and so the
# parsing has one home instead of three grep pipelines that did not have
# to agree with each other.
#
# Emits one tab-separated record per dependency on THIS repository:
#   OK <rev> <name>        a revision it can read
#   NOREV <name> <where>   a git dependency on us carrying none
read -r -d '' MANIFEST_PINS <<'AWK' || true
function strip(line,   i, c, q, out) {
    # Comments, with the quote state tracked: a `#` inside a string is
    # not a comment, and a trailing `# was rev = "..."` note otherwise
    # became a second revision and a false "two trees" finding.
    q = ""; out = ""
    for (i = 1; i <= length(line); i++) {
        c = substr(line, i, 1)
        if (q != "") { out = out c; if (c == q) q = ""; continue }
        if (c == "\"" || c == "'") { q = c; out = out c; continue }
        if (c == "#") break
        out = out c
    }
    return out
}
# Anchored past the name: `gzapi-org/interweave-tools` is a different
# repository, and so is `other-org/InterWeave`.
function ours(t) { return tolower(t) ~ /git[ \t]*=[ \t]*["'][^"']*gzapi-org\/interweave([^a-z0-9_-]|$)/ }
function revof(t,   m) {
    if (match(t, /[Rr][Ee][Vv][ \t]*=[ \t]*["'][0-9a-fA-F]{7,40}["']/)) {
        m = substr(t, RSTART, RLENGTH)
        sub(/^[^"']*["']/, "", m); sub(/["'].*$/, "", m)
        return m
    }
    return ""
}
function flush() {
    if (secname != "" && secours) {
        if (secrev != "") print "OK\t" secrev "\t" secname
        else print "NOREV\t" secname "\t[" sechdr "]"
    }
    secname = ""; secours = 0; secrev = ""; sechdr = ""
}
{ line = strip($0) }
line ~ /^[ \t]*\[/ {
    flush()
    # Every table spelling, not just `[dependencies.x]`: a `[target.
    # 'cfg(unix)'.dependencies.x]` or `[workspace.dependencies.x]` pin
    # was invisible to all three of the checks this replaces.
    insec = (line ~ /dependencies\.[A-Za-z0-9_.-]+[ \t]*\]/)
    if (insec) {
        sechdr = line
        sub(/^[ \t]*\[/, "", sechdr); sub(/\][ \t]*$/, "", sechdr)
        secname = sechdr; sub(/.*\./, "", secname)
    }
    next
}
insec {
    if (ours(line)) secours = 1
    r = revof(line); if (r != "") secrev = r
    next
}
ours(line) {
    n = line; sub(/[ \t]*=.*/, "", n); gsub(/[ \t]/, "", n)
    r = revof(line)
    if (r != "") print "OK\t" r "\t" n
    else print "NOREV\t" n "\t" line
}
END { flush() }
AWK

if (( PROVENANCE == 1 )); then
    # NEITHER OF THESE IS A PASS. `--root` may point at an exported tree
    # with no `.git`, and there is no fallback here the way the lock
    # phase falls back to `find`: where a pin came from is a question
    # only history answers. Skipping it quietly and then printing "the
    # pins are accounted for" is the silent pass this whole file exists
    # to remove, so the caller says `--no-provenance` if that is what
    # they meant (review, PR #110).
    #
    # AND origin/main MUST RESOLVE. Without it `git merge-base
    # --is-ancestor` exits 128 for a bad revision, which the `2>/dev/null`
    # below turns into the same answer as 1 -- "not an ancestor" -- so in
    # a checkout whose remote is named anything else, every valid pin is
    # reported as an unmerged feature tip and the run exits 1 with a
    # finding it invented. Measured: exit 128, `fatal: Not a valid object
    # name origin/main` (review, PR #110).
    if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
        echo "check_spike_locks: not a git checkout, so where each pin came from cannot be asked." >&2
        echo "  Run this in a checkout, or pass --no-provenance to say you meant to skip it." >&2
        exit 2
    elif ! git rev-parse --verify --quiet origin/main >/dev/null 2>&1; then
        echo "check_spike_locks: no origin/main in this checkout, so a pin cannot be placed" >&2
        echo "  against it. Fetch it, or pass --no-provenance." >&2
        exit 2
    # A SHALLOW CLONE CANNOT ANSWER REACHABILITY, and it does not say so.
    # `--is-ancestor` is a reachability query; with the connecting
    # history absent it returns 1 -- the same answer as a genuine
    # non-ancestor. A depth-1 clone followed by a depth-1 fetch of the
    # pin reaches exactly that state: `git cat-file -e` succeeds, so the
    # missing-object branch below lets it through, and every pin is then
    # reported as an unmerged feature tip (review, PR #110). The grafted
    # boundary is what makes the question unanswerable, so this is exit 2
    # and never a finding.
    elif [[ "$( git rev-parse --is-shallow-repository 2>/dev/null )" == "true" ]]; then
        echo "check_spike_locks: this is a shallow clone, so whether a pin is an ancestor of" >&2
        echo "  origin/main cannot be answered -- the connecting history is not here, and the" >&2
        echo "  answer would be indistinguishable from a genuine non-ancestor. Unshallow it" >&2
        echo "  (in CI: the checkout step's fetch-depth), or pass --no-provenance." >&2
        exit 2
    else
        pinned=0
        bad=0
        while IFS= read -r manifest; do
            [[ -n "$manifest" ]] || continue
            spike_dir="${manifest%/harness/Cargo.toml}"
            # TWO PASSES, BECAUSE THEY ANSWER DIFFERENT QUESTIONS. Whether
            # a DEPENDENCY carries a revision is per dependency -- one
            # unpinned entry must not hide behind a pinned sibling. Where
            # a REVISION came from is per revision: four dependencies at
            # one commit are one tree and one trace line, not four.
            seen_revs=()
            while IFS=$'\t' read -r kind field where; do
                [[ -n "$kind" ]] || continue
                if [[ "$kind" == NOREV ]]; then
                    echo "check_spike_locks: $manifest depends on this repository by git with no" >&2
                    echo "  revision this check can read:" >&2
                    printf '    %s  %s\n' "$field" "$where" >&2
                    bad=$((bad + 1))
                    continue
                fi
                [[ "$kind" == OK ]] || continue
                seen_revs+=( "$field" )
            done < <( awk "$MANIFEST_PINS" "$manifest" )
            while IFS= read -r rev; do
                [[ -n "$rev" ]] || continue
                pinned=$((pinned + 1))
                if ! git cat-file -e "${rev}^{commit}" 2>/dev/null; then
                    echo "check_spike_locks: $spike_dir pins $rev, which this clone does not have." >&2
                    echo "  The commit was never pushed, this is a partial clone, or the URL names" >&2
                    echo "  a repository that is not this one. A shallow clone is caught earlier and" >&2
                    echo "  says so; this is not that." >&2
                    exit 2
                fi
                if ! git merge-base --is-ancestor "$rev" origin/main 2>/dev/null; then
                    echo "check_spike_locks: $spike_dir pins $rev, which is NOT an ancestor of origin/main." >&2
                    echo "  A pin that never merged is a feature-branch tip, not a tree anyone can return to." >&2
                    bad=$((bad + 1))
                    continue
                fi
                # THE SEARCH STARTS FROM THE SPIKE'S HISTORY, not
                # from the pin. A pin derived from a DATE is, by
                # construction, the parent of no recording commit --
                # that is the defect it has -- so walking outward from
                # it can only come up empty and says nothing about
                # where the right tree is (architect-cto, 2026-09-20).
                # Enumerating the spike's own commits and asking which
                # one the pin belongs to answers the question the rule
                # actually poses.
                #
                # TWO SHAPES, one meaning: the pin is the tree the run
                # built against.
                #   1. A recording commit that changes NO production
                #      crate resolved the crates by path from its
                #      parent, so the pin is that parent.
                #   2. One that ALSO changes a crate measured the code
                #      it landed with, so the pin is that commit
                #      itself.
                #
                # A MERGE IS NOT A RECORDING, and two things keep one
                # out. `git show --name-only` prints NOTHING for a
                # merge, so an empty file list would compute zero files
                # outside the spike and pass -- which is what made an
                # earlier version of this phase vacuous, when it walked
                # the pin's children (measured on spike-002's pin,
                # whose first child is the merge 1a345aa).
                #
                # THE PATHSPEC DOES MOST OF THE WORK NOW, and saying
                # `--no-merges` is what excludes merges would be false:
                # git's default history simplification drops a merge
                # that is TREESAME to a parent, so a path-filtered walk
                # already omits ordinary ones. Measured on this
                # repository, with and without the flag: 34/34 for
                # spike-002, 43/43 for spike-003, 149/149 for spike-004
                # -- it removes nothing (review, PR #110).
                #
                # THE FLAG STILL EARNS ITS PLACE, for the merge the
                # pathspec does NOT drop: an evil merge, whose tree
                # differs from both parents under this spike's
                # directory, is not TREESAME and IS listed. Its file
                # list is non-empty and confined to the spike, so
                # without `--no-merges` it would be accepted as a
                # recording commit. The self-test builds exactly that
                # merge, so deleting the flag fails it.
                #
                # And a commit that touches no file is refused below in
                # either case, because an empty list is not evidence.
                #
                # WHICH COMMIT LAST RAN IS NOT MECHANICAL -- it is read
                # from the message, and this phase does not try. It
                # asks only whether the pin is ONE OF the trees a
                # commit in this spike's history points at. A pin that
                # passes may still be the wrong run's tree; the
                # reproduction run cited beside it is what settles
                # that.
                recording=""
                shape=""
                pin_sha="$( git rev-parse "$rev" )"
                while IFS= read -r candidate; do
                    [[ -n "$candidate" ]] || continue
                    files="$( git show --stat --format='' --name-only "$candidate" | grep -v '^$' )"
                    [[ -n "$files" ]] || continue
                    outside="$( printf '%s\n' "$files" | grep -cv "^$spike_dir/" )"
                    if [[ "$outside" -eq 0 ]]; then
                        parent="$( git rev-parse --verify --quiet "${candidate}^" 2>/dev/null || true )"
                        if [[ -n "$parent" && "$parent" == "$pin_sha" ]]; then
                            recording="$candidate"; shape="parent of"; break
                        fi
                    elif printf '%s\n' "$files" | grep -q '^crates/'; then
                        if [[ "$candidate" == "$pin_sha" ]]; then
                            recording="$candidate"; shape="itself,"; break
                        fi
                    fi
                done < <( git rev-list --no-merges origin/main -- "$spike_dir/" 2>/dev/null )
                if [[ -z "$recording" ]]; then
                    echo "check_spike_locks: $spike_dir pins $rev, which no commit in that" >&2
                    echo "  spike's history points at. The pin is the tree the last recorded run" >&2
                    echo "  built against — see SPIKES.md's preamble — reached from the spike's" >&2
                    echo "  OWN history: a recording commit that changes no crate points at its" >&2
                    echo "  parent, one that changes a crate points at itself. A pin derived from" >&2
                    echo "  a DATE lands here, because a run is recorded on a branch days before" >&2
                    echo "  it merges." >&2
                    bad=$((bad + 1))
                    continue
                fi
                echo "check_spike_locks: $spike_dir pins $rev — on origin/main, $shape ${recording:0:7}."
            # ONE READER, NOT A PILE OF GREPS. Everything above about a
            # pin -- is this dependency ours, does it carry a revision,
            # where is it written -- is answered by one pass over the
            # manifest, because asking those questions with separate
            # `grep`s meant they did not have to describe the same
            # DEPENDENCY. That produced three defects in three rounds:
            # a manifest-wide "is there any rev" that let an unpinned
            # dependency hide behind a pinned sibling; a refusal that
            # fired when ANY multi-line table existed beside ANY pin of
            # ours; and a `{`-only filter that made
            # `[target.'"'"'cfg(unix)'"'"'.dependencies.x]` and
            # `[workspace.dependencies.x]` invisible to all of them
            # (review, PR #110, in the audit of the two commits that
            # were merged unreviewed).
            #
            # It reads both dependency forms -- an inline table on one
            # line, and a `[...dependencies.NAME]` block whose `git` and
            # `rev` are on separate lines -- strips comments with the
            # quote state tracked, and anchors the URL on `gzapi-org/
            # interweave` followed by a non-name character, so
            # `gzapi-org/interweave-tools` is NOT us and
            # `other-org/InterWeave` is NOT us.
            done < <( printf '%s\n' "${seen_revs[@]}" | grep -v '^$' | sort -u )
            # ONE HARNESS, ONE TREE. Several dependencies pinned at
            # DIFFERENT revisions were each traced individually and the
            # run said the pins were accounted for, for a harness
            # building against two trees. "A frozen spike is frozen all
            # the way down" (`SPIKES.md`) is one tree, not a set.
            #
            # RESOLVED COMMITS, NOT STRINGS: `94f72cc`, `94f72cc4e4...`
            # and `94F72CC4E4...` are one commit written three ways, and
            # counting strings called a correctly-pinned harness a
            # two-tree one (measured while adding the spellings case).
            spike_revs="$( printf '%s\n' "${seen_revs[@]}" \
                           | while IFS= read -r r; do
                                 [[ -n "$r" ]] && git rev-parse --verify --quiet "${r}^{commit}"
                             done | sort -u | grep -c . )"
            if (( spike_revs > 1 )); then
                echo "check_spike_locks: $spike_dir pins this repository at $spike_revs different" >&2
                echo "  revisions. A harness builds against ONE tree; the evidence cannot name two." >&2
                bad=$((bad + 1))
            fi
        done < <( if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
                      git ls-files -- 'spikes/*/harness/Cargo.toml'
                  fi )
        if (( bad > 0 )); then
            echo "check_spike_locks: $bad pin(s) have provenance this repository cannot account for." >&2
            exit 1
        fi
        if (( pinned == 0 )); then
            echo "check_spike_locks: no harness pins a revision of this repository; nothing to trace."
        fi
    fi
fi

# PHASE THREE. Only reached when every lock resolved: a stale lock makes
# `cargo check --locked` fail for the reason phase one already named, so
# running it would report the same finding twice under a worse
# description.
if (( BUILD == 0 )); then
    if (( PROVENANCE == 1 )); then
        echo "check_spike_locks: OK — --no-build; the pins are accounted for, the harnesses were not compiled."
    else
        echo "check_spike_locks: OK — --no-build --no-provenance; the harnesses were not compiled and the pins were not traced."
    fi
    exit 0
fi

broken=0
for lock in "${locks[@]}"; do
    dir="$( dirname -- "$lock" )"
    if output="$( cd "$dir" && "${CARGO:-cargo}" check --locked --quiet 2>&1 )"; then
        echo "check_spike_locks: $dir compiles at its pinned revisions."
    # A KILLED COMPILER IS NOT A BAD PIN. When rustc starts and is then
    # killed -- an OOM on a loaded runner is the realistic one -- cargo
    # prints `error: could not compile ...` followed by `process didn't
    # exit successfully: ... (signal: 9, SIGKILL: kill)`, with no rustc
    # diagnostic anywhere. The broad sentinel alone read that as a
    # source failure and exited 1, which on a required CI job tells the
    # author to investigate or move a pin that is fine (review, PR
    # #110). Measured with a rustc wrapper that SIGKILLs itself.
    #
    # ORDER MATTERS HERE. `error[E` is checked first because it is
    # rustc's own diagnostic and settles the question; only then is a
    # signal allowed to reclassify a `could not compile` as something
    # cargo never got an answer for.
    elif printf '%s' "$output" | grep -q -e 'error\[E'; then
        echo "check_spike_locks: $dir DOES NOT COMPILE at the revisions it pins." >&2
        # rustc's own first lines name the import or the type; the rest
        # is a wall this guard's output does not need.
        printf '%s\n' "$output" | grep -e 'error\[E' -e '^error' | head -3 | sed 's/^/    /' >&2
        broken=$((broken + 1))
    # AND A SIGNAL ONLY SPEAKS WHEN NOTHING ELSE DID. `error[E` above
    # catches CODED diagnostics, but plenty of real rustc errors carry
    # no `[Ennnn]` -- a denied lint, a link failure, some parse errors.
    # One of those on the pinned source, in a run where any process also
    # died (a dependency OOM-killed on a loaded runner, which is the
    # scenario this branch exists for), would otherwise be reported as
    # an environment failure and the real finding lost. So the signal
    # branch also requires that cargo printed no `error:` line other
    # than its own `could not compile` summary (audit, PR #110).
    elif printf '%s' "$output" | grep -q '(signal:' \
         && [[ "$( printf '%s\n' "$output" | grep -c '^error' )" -le 1 ]]; then
        echo "check_spike_locks: cannot ask whether $dir compiles — the compiler was killed." >&2
        printf '%s\n' "$output" | grep -e '(signal:' | head -2 | sed 's/^/    /' >&2
        exit 2
    elif printf '%s' "$output" | grep -q -e 'could not compile'; then
        echo "check_spike_locks: $dir DOES NOT COMPILE at the revisions it pins." >&2
        printf '%s\n' "$output" | grep -e '^error' | head -3 | sed 's/^/    /' >&2
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

THE PIN IS NOT DERIVED FROM A DATE. It is the tree the last recorded
run built against, found from the spike's OWN history (`SPIKES.md`
preamble):

  1. Read the spike's record and its git log for the last commit whose
     message records a RUN of the harness -- not a lock refresh, not a
     note on the record.
  2. Two shapes. That commit changes no production crate: the pin is its
     PARENT, because the crates it resolved by path were its parent's.
     It also changes a crate: the pin is that COMMIT, because the run
     measured the code it landed with. Consecutive run-recording commits
     are a chain, and the pin is the tree the chain sits on.
  3. Prove it. Run the harness at the pin and compare against the
     recorded observations; cite the reproduction beside the pin. A pin
     that compiles and fails a recorded row is the wrong tree, and this
     check cannot tell you which -- it only reports that the pin is one
     of the trees the spike's history points at.

A `git rev-list --before=<date>` derivation is what put three of these
pins wrong. A run is recorded on a feature branch and merges days later,
so `main`'s head on the run's own date is not the tree it built against.

Moving a pin is not a free edit. Under a FROZEN regime it may move only
to correct a derivation that was wrong; under a release gate only in the
change that re-runs and re-records. Read the spike's own regime first.
EOF
    exit 1
fi

# THE OK LINE CLAIMS ONLY WHAT RAN. `--no-build` narrows its own line a
# screen above; `--no-provenance` did not, so a run that traced nothing
# still ended with "the pins are accounted for" (review, PR #110).
if (( PROVENANCE == 1 )); then
    echo "check_spike_locks: OK — ${#locks[@]} spike harness(es) resolve and compile at their pinned revisions; the pins are accounted for."
else
    echo "check_spike_locks: OK — ${#locks[@]} spike harness(es) resolve and compile; the pins were NOT traced (--no-provenance)."
fi
# EXPLICIT, so the script's status is not the final `echo`'s -- it is
# right for a closed or unwritable stdout, where the echo fails without
# a signal. It does NOT rescue SIGPIPE: a review measured
# `check_spike_locks.sh | head -1` on 200k lines of output at rc 141
# both with this line and without it, because the signal kills the shell
# before `exit 0` runs. An earlier version of this comment claimed
# otherwise (review, PR #107).
exit 0
