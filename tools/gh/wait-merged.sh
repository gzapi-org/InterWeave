#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/gh/wait-merged.sh
#
# >>> help
# Block until a PR reaches a terminal state, then exit saying which.
#
# The point is the EXIT, not the waiting: run it in the background and
# the exit is a callback. Nothing has to sit in a loop asking "has it
# merged yet" between other work, and nobody has to remember to check.
#
#   tools/gh/wait-merged.sh 429            # blocks; exits when decided
#
# GitHub offers a local client no push channel — a webhook needs
# somewhere to arrive — so the polling has to happen somewhere. Putting
# it in a detached process that exits once is the difference between one
# notification and a checking habit.
#
# CLOSED-without-merge exits non-zero deliberately. It is the case that
# looks exactly like "still waiting" if you only watch for success, and
# it is the one where returning to main would be wrong.
#
# This script does NOT touch your working tree. It runs detached, and a
# checkout underneath a session that is mid-edit is a race with nothing
# to catch it. It tells you the state; moving branches stays a
# deliberate act.
#
# Usage:
#   tools/gh/wait-merged.sh <pr-number> [options]
#
# Options:
#   --interval <duration>  between checks (default 30; the API is rate
#                          limited and merges take minutes, not seconds)
#   --timeout <duration>   give up after this long (default 2400 = 40m)
#
# Durations take an optional unit — 90, 90s, 10m, 2h. A bare number is
# SECONDS, so anything written before units existed still means what it
# meant. pr-review-status.sh accepts exactly the same forms.
#   -q, --quiet            no progress lines on stderr
#   -h, --help             this text
#
# Every run ends with ONE line on stdout naming the outcome and the code
# it exits with — `PR #431 MERGED — safe to return to main (exit 0)` —
# so the answer is legible to a person and branchable by a caller. Usage
# errors still go to stderr; they are not results.
#
# Exit codes:
#   0  merged
#   2  invocation problem, or the PR could not be read repeatedly
#   3  closed WITHOUT merging — do not treat as done
#   4  still open when the timeout expired
#   5  blocked and going nowhere — a check concluded badly, or the branch
#      conflicts with its base. BLOCKED with checks merely PENDING is the
#      normal path and keeps waiting.
#   6  stalled for a reason OUTSIDE the PR — GitHub Actions is degraded,
#      the head commit has no workflow run at all (a lost push webhook),
#      or the included Actions allowance is spent. Distinguished from 4
#      because 4 means "nobody knows" and 6 names the cause.
# <<< help

set -uo pipefail

PR=""
INTERVAL=30
TIMEOUT=2400
QUIET=0

die() { echo "wait-merged: $*" >&2; exit 2; }

need_operand() {
    [[ $# -ge 2 ]] || die "$1 needs a value (try --help)."
}

# Durations take an optional unit: 90, 90s, 10m, 2h. A BARE NUMBER IS
# SECONDS — which is what every existing invocation already meant, so
# nothing changes for a caller that passed one.
#
# pr-review-status.sh carries an identical copy. That is deliberate:
# these are standalone scripts with no shared library, and a divergence
# in what they accept is exactly the confusion the units were added to
# remove. Both suites assert the same table, so a drift fails a test.
as_seconds() {
    local flag="$1" raw="$2" n
    case "$raw" in
        ''|*[!0-9smh]*) die "$flag needs a duration like 90, 90s, 10m or 2h, got '$raw'." ;;
    esac
    n="${raw%[smh]}"
    [[ "$n" =~ ^[0-9]+$ ]] \
        || die "$flag needs a duration like 90, 90s, 10m or 2h, got '$raw'."
    case "$raw" in
        *h) echo $(( n * 3600 )) ;;
        *m) echo $(( n * 60 )) ;;
        *)  echo "$n" ;;
    esac
}

want_positive() {
    [[ "$2" =~ ^[1-9][0-9]*$ ]] \
        || die "$1 must be greater than zero, got '$3'."
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        # `as_seconds` dies inside a command substitution, which only
        # kills the SUBSHELL — without propagating the status here the
        # script would sail on with an empty interval.
        --interval) need_operand "$@"; INTERVAL="$(as_seconds "$1" "$2")" || exit 2
                    want_positive "$1" "$INTERVAL" "$2"; shift 2 ;;
        --timeout)  need_operand "$@"; TIMEOUT="$(as_seconds "$1" "$2")" || exit 2
                    want_positive "$1" "$TIMEOUT" "$2"; shift 2 ;;
        -q|--quiet) QUIET=1; shift ;;
        -h|--help)
            sed -n '/^# >>> help$/,/^# <<< help$/p' "$0" \
                | sed '1d;$d' | sed 's/^# \{0,1\}//'
            exit 0 ;;
        -*) die "unknown option '$1' (try --help)" ;;
        *)
            [[ -z "$PR" ]] || die "one PR number at a time (got '$PR' and '$1')"
            PR="$1"; shift ;;
    esac
done

[[ -n "$PR" ]] || die "a PR number is required (try --help)"
[[ "$PR" =~ ^[1-9][0-9]*$ ]] || die "'$PR' is not a PR number."

for bin in gh jq; do
    command -v "$bin" >/dev/null 2>&1 || die "$bin is required but not installed."
done

note() { [[ "$QUIET" -eq 1 ]] || echo "wait-merged: $*" >&2; }

# Every terminal answer leaves through here: one line on STDOUT naming
# the outcome AND the code it exits with, then the exit itself.
#
# Both halves are load-bearing. The exit code is what a caller branches
# on — a background watcher reports it and nothing else — while the
# line is what a HUMAN reads, in a terminal or in the task output, and
# "exit 5" alone says nothing about which of the two blocked cases it
# was. And it is stdout for every verdict, not just the happy ones:
# the expiry and unreadable answers used to go to stderr, so a caller
# redirecting stdout to capture the result got the successes and lost
# exactly the failures it most needed to see.
verdict() {
    local code="$1"; shift
    printf 'PR #%s %s (exit %s)\n' "$PR" "$*" "$code"
    exit "$code"
}

# ── Why is this PR going nowhere? ────────────────────────────────────
#
# `STILL OPEN — state unknown` is this tool admitting it spent the whole
# timeout and learned nothing. Every stall observed on 2026-08-06 ended
# there, and in each case the answer was available the entire time from
# somewhere OTHER than the PR itself:
#
#   * an Actions outage — jobs cancelled with zero steps, or never
#     scheduled at all;
#   * a push whose webhook was lost, leaving the head SHA with NO run —
#     which `gh pr checks` reports as "no checks reported", reading like
#     nothing ran rather than like a run went missing;
#   * the included Actions allowance spent, so green code cannot merge.
#
# These are deliberately LAZY. The normal path costs exactly what it did
# before — one `gh pr view` per poll — because a healthy watch should not
# pay for an outage probe every thirty seconds. They run only when the
# watch is about to give up, or when a PR has demonstrably no run at all.
#
# Every probe is best-effort: a failing diagnosis must never turn a
# useful verdict into a crash, so each swallows its own errors and the
# caller falls through to the original answer.

# Actions component status, straight from GitHub's public status page.
# Unauthenticated, so it works even when the token is the problem.
actions_degraded() {
    command -v curl >/dev/null 2>&1 || return 1
    local s
    s="$(curl -fsS --max-time 10 https://www.githubstatus.com/api/v2/summary.json 2>/dev/null)" || return 1
    printf '%s' "$s" | jq -e -r '
        [.components[]? | select(.name == "Actions") | .status] | first
        | select(. != null and . != "operational")' 2>/dev/null
}

# Has the head commit got ANY workflow run? A lost push webhook leaves a
# PR whose head SHA nothing ever built, and it will never resolve on its
# own — the fix is to re-trigger, and (per CLAUDE.md) to RE-ARM after,
# because close/reopen silently drops auto-merge.
head_sha_has_no_run() {
    local sha runs workflows any_runs
    sha="$(gh pr view "$PR" --json headRefOid -q .headRefOid 2>/dev/null)" || return 1
    [[ -n "$sha" ]] || return 1

    # ZERO RUNS ONLY MEANS A LOST WEBHOOK IF A RUN WAS EVER EXPECTED.
    # Two things must hold before that inference is worth making, and
    # NEITHER is proof — see the residual below.
    #
    # 1. The repository has workflow files at all. One with none produces
    #    no runs by design; this repository has none today, and CLAUDE.md
    #    says so. Without this, every watch here ends at exit 6 with
    #    re-trigger instructions for a run that was never coming.
    # 2. The repository has actually produced a run at some point. A tree
    #    whose workflows have never fired is indistinguishable from one
    #    with no CI, and is likelier to be misconfigured than webhooked.
    #
    # RESIDUAL, stated because it cannot be closed here: neither test
    # proves a workflow applies to THIS push. A branch- or path-filtered
    # workflow legitimately skips a commit, leaving workflows > 0, runs
    # elsewhere > 0, and zero runs for this head — the same signature as
    # a lost webhook. Deciding between them needs the filter expressions
    # evaluated against the changed paths, which is a real matcher, not a
    # shell probe. The verdict text therefore names both causes rather
    # than asserting the webhook.
    workflows="$(gh api "repos/{owner}/{repo}/actions/workflows?per_page=1"         -q '.total_count' 2>/dev/null)" || return 1
    [[ "$workflows" =~ ^[0-9]+$ ]] || return 1
    (( workflows > 0 )) || return 1

    any_runs="$(gh api "repos/{owner}/{repo}/actions/runs?per_page=1"         -q '.total_count' 2>/dev/null)" || return 1
    [[ "$any_runs" =~ ^[0-9]+$ ]] || return 1
    (( any_runs > 0 )) || return 1

    runs="$(gh api "repos/{owner}/{repo}/actions/runs?head_sha=$sha&per_page=1"         -q '.total_count' 2>/dev/null)" || return 1
    [[ "$runs" == "0" ]]
}

# Past the INCLUDED allowance. The billing API reports usage, never the
# plan's limit, so the limit is CONFIGURED rather than discovered:
# $INTERWEAVE_ACTIONS_INCLUDED_MINUTES, set in .claude/settings.json. No plan
# size is hardcoded anywhere in this repo.
#
# netAmount > 0 is still checked, but it is NOT sufficient — and this
# function relied on it alone until 2026-08-07. A non-zero net means
# overage is actually being BILLED, which only happens on a plan that
# purchases overage. A plan that does not is never billed: GitHub simply
# stops handing out runners while net stays 0. On this repo's plan that
# made the allowance branch of exit 6 unreachable, so the one state it
# existed to name could never be named. Usage-against-limit is the check
# that fires there, and it needs the setting.
#
# `netAmount: 0` therefore means nothing is being billed — never that the
# allowance is intact. Same correction actions-health.sh took the same
# day; the two now agree.
actions_allowance_spent() {
    local owner usage net mins included
    included="${INTERWEAVE_ACTIONS_INCLUDED_MINUTES:-}"

    owner="$(gh repo view --json owner -q .owner.login 2>/dev/null)" || return 1
    usage="$(gh api "/organizations/$owner/settings/billing/usage" 2>/dev/null)" || return 1
    [[ -n "$usage" ]] || return 1

    net="$(jq -r '[.usageItems[]? | select(.product == "actions") | .netAmount] | add // 0' \
        <<<"$usage" 2>/dev/null || echo 0)"
    awk -v n="$net" 'BEGIN { exit !(n > 0) }' && return 0

    # No configured allowance: usage alone cannot say what is left, so
    # decline to guess rather than report a false "spent".
    [[ -n "$included" ]] || return 1
    awk -v i="$included" 'BEGIN { exit !(i + 0 > 0) }' || return 1

    mins="$(jq -r '[.usageItems[]? | select(.product == "actions" and (.unitType == "Minutes")) | .quantity] | add // 0' \
        <<<"$usage" 2>/dev/null || echo 0)"
    awk -v m="$mins" -v i="$included" 'BEGIN { exit !(m + 0 >= i + 0) }'
}

# Called where the watch would otherwise give up. Turns the useless
# answer into a named one wherever the cause is knowable.
#
# $1 — "1" when the PR has reported NO required checks at all. Only then
# is the missing-run probe meaningful: if required checks are reporting,
# a run demonstrably exists, and asking the runs API anyway invites a
# contradiction. It did exactly that during development — a PR with
# healthy pending checks was reported as having no run, because the probe
# was consulted unconditionally at expiry.
diagnose_stall() {
    local no_checks="${1:-0}" st
    if st="$(actions_degraded)"; then
        verdict 6 "STALLED — GitHub Actions is $st (githubstatus.com); the PR is fine, the platform is not"
    fi
    if [[ "$no_checks" == "1" ]] && head_sha_has_no_run; then
        verdict 6 "STALLED — no workflow run exists for this PR's head commit. Either the push webhook was lost, or a branch/path filter skipped every workflow for this commit. Check which before re-triggering; if you re-trigger, RE-ARM auto-merge afterwards"
    fi
    if actions_allowance_spent; then
        verdict 6 "STALLED — the included Actions allowance is spent; green code will not merge until it resets"
    fi
}

# A single failed lookup is a blip — a dropped connection, a rate-limit
# pause — and killing the watch over one would be worse than useless,
# because the caller is asleep and would learn nothing. Several in a row
# is a different thing: a deleted PR, a revoked token, no network. Then
# it has to say so rather than wait out the timeout in silence.
MAX_CONSECUTIVE_FAILURES=5
failures=0
elapsed=0
# Polls with ZERO required checks before the missing-run diagnosis runs.
# Three is comfortably past the window in which GitHub is simply still
# registering a fresh push, without making the wait pointless.
EMPTY_POLLS_BEFORE_DIAGNOSIS=3
empty_polls=0

note "watching #$PR (every ${INTERVAL}s, giving up after ${TIMEOUT}s)"

# Say so IMMEDIATELY if nothing is going to merge this. A PR that is
# neither queued nor auto-merge-armed will sit green forever, and the
# watch would spend its whole budget to report "still open" — true, and
# useless. This is the other half of what went wrong on 2026-08-06:
# repeated queue evictions left PRs armed-looking but unqueued, and it
# took three expired watches to notice.
#
# A note rather than an exit: arming moments after starting the watch is
# a perfectly reasonable order to do things in, and refusing would make
# that impossible.
arming="$(gh api graphql -f query='
    query($owner: String!, $repo: String!, $number: Int!) {
      repository(owner: $owner, name: $repo) {
        pullRequest(number: $number) {
          state
          autoMergeRequest { enabledAt }
          mergeQueueEntry { state }
        }
      }
    }' -F owner="${GH_REPO_OWNER:-$(gh repo view --json owner --jq .owner.login 2>/dev/null)}" \
       -F repo="${GH_REPO_NAME:-$(gh repo view --json name --jq .name 2>/dev/null)}" \
       -F number="$PR" \
    --jq '.data.repository.pullRequest
          | if .state != "OPEN" then "decided"
            elif .mergeQueueEntry then "queued"
            elif .autoMergeRequest then "armed"
            else "idle" end' 2>/dev/null || echo unknown)"
if [[ "$arming" == "idle" ]]; then
    note "#$PR is neither queued nor auto-merge-armed — nothing will merge it."
    note "  arm it:  gh pr merge $PR --merge --auto"
fi

while true; do
    if payload="$(gh pr view "$PR" --json state,mergeStateStatus 2>/dev/null)" \
        && [[ -n "$payload" ]]; then
        failures=0
        state="$(printf '%s' "$payload" | jq -r '.state // ""')"
        merge_state="$(printf '%s' "$payload" | jq -r '.mergeStateStatus // ""')"

        # REQUIRED checks only, and read through `gh pr checks` rather
        # than the raw rollup. Two reasons, and the second is not
        # obvious:
        #
        #   * an OPTIONAL check that fails does not stop the queue from
        #     merging, so treating it as terminal would report a healthy
        #     PR as dead;
        # `cancel` counts as terminal alongside `fail`. A cancelled
        # required check cannot let the PR merge without a re-run, so
        # treating it as merely pending waits out the whole timeout for
        # nothing — which is exactly what happened here on 2026-08-06:
        # an Actions outage evicted PRs from the queue, GitHub cancelled
        # 13 jobs in the abandoned merge-group run, and three watches
        # expired at 40 minutes reporting "state unknown" instead of
        # naming a cancelled check on the first poll. (`gh pr checks`
        # documents the full set: pass, fail, pending, skipping, cancel.)
        #
        #   * the rollup mixes CheckRun entries (`conclusion`, `name`)
        #     with legacy StatusContext ones (`state`, `context`), and a
        #     filter written for the first silently ignores the second —
        #     a failing required status would then be invisible and the
        #     watch would run to timeout. `bucket` normalises both.
        #
        # gh exits non-zero when checks are failing or pending, which is
        # a status here and not an error, so the exit code is discarded
        # and an unreadable result simply means "nothing failing yet".
        checks_json="$( { gh pr checks "$PR" --required --json name,bucket 2>/dev/null \
            || true; } )"
        failing="$(printf '%s' "$checks_json" \
            | jq -r '[.[]? | select(.bucket == "fail" or .bucket == "cancel")
                      | .name] | join(", ")' 2>/dev/null || true)"

        # ZERO required checks is not the same as "checks pending". It is
        # what a PR looks like when its head SHA has no workflow run at
        # all — the lost-webhook case — and that never resolves on its
        # own, so waiting the full timeout to say so is precisely the
        # waste this tool exists to avoid.
        #
        # Confirmed rather than assumed, and only after several polls:
        # there is a legitimate window right after a push where GitHub
        # has not yet registered the run, and firing on the first empty
        # poll would misreport every healthy watch.
        # A TERMINAL STATE OUTRANKS ANY DIAGNOSIS. A PR can merge on the
        # very poll where checks still read empty, and diagnose_stall
        # exits 6 without returning — so diagnosing first would report a
        # successfully merged PR as stalled and tell the caller not to
        # return to main. The state is what actually happened; the
        # diagnosis is only ever an explanation for why nothing has.
        case "$state" in
            MERGED)
                verdict 0 "MERGED — safe to return to main" ;;
            CLOSED)
                verdict 3 "CLOSED WITHOUT MERGING — do not return to main" ;;
        esac

        if [[ "$(printf '%s' "$checks_json" | jq -r 'length' 2>/dev/null || echo 1)" == "0" ]]; then
            empty_polls=$(( empty_polls + 1 ))
            if (( empty_polls >= EMPTY_POLLS_BEFORE_DIAGNOSIS )); then
                note "#$PR has reported no required checks for $empty_polls polls; diagnosing"
                diagnose_stall 1
            fi
        else
            empty_polls=0
        fi

        # BLOCKED is the NORMAL state while checks run, so it is not by
        # itself a reason to stop. What ends the watch is a PR that will
        # not merge as it stands: a check that has already concluded
        # badly, or a base conflict. Waiting either of those out to the
        # timeout would spend forty minutes to report nothing, when the
        # actionable fact was available in the first poll.
        if [[ -n "$failing" ]]; then
            verdict 5 "BLOCKED — these required checks failed: $failing"
        fi
        if [[ "$merge_state" == "DIRTY" ]]; then
            verdict 5 "BLOCKED — conflicts with the base; fold origin/main in"
        fi
        [[ "$merge_state" == "BLOCKED" ]] && note "#$PR blocked, checks still pending"
    else
        failures=$((failures + 1))
        note "could not read #$PR ($failures/$MAX_CONSECUTIVE_FAILURES)"
        if (( failures >= MAX_CONSECUTIVE_FAILURES )); then
            verdict 2 "UNREADABLE — $MAX_CONSECUTIVE_FAILURES lookups failed in a row; giving up"
        fi
    fi

    remaining=$(( TIMEOUT - elapsed ))
    if (( remaining <= 0 )); then
        # Before admitting defeat, ask the sources that know something
        # the PR does not. Checks were reporting, so a missing run is not
        # among the possibilities here.
        diagnose_stall 0
        verdict 4 "STILL OPEN after ${TIMEOUT}s — watch expired, state unknown"
    fi
    # Never sleep past the deadline, and never cut the watch short of
    # it. `--interval 60 --timeout 30` used to expire on the FIRST poll
    # while claiming it had waited 30s; the nap is clamped to what is
    # left so the advertised timeout is the one actually observed.
    nap=$(( INTERVAL < remaining ? INTERVAL : remaining ))
    sleep "$nap"
    elapsed=$((elapsed + nap))
done
