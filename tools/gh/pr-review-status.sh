#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/gh/pr-review-status.sh
#
# >>> help
# Has anyone OTHER THAN THE AUTHOR actually reviewed the code that is at
# the head of this PR right now? A raw `gh pr view` cannot answer that.
#
# Two GitHub behaviours make the obvious reading wrong, and both bit
# this repo:
#
#   1. Replying to a review thread (addPullRequestReviewThreadReply)
#      creates a REVIEW object with an empty body, authored by you.
#      Three self-replies look like three new reviews. On PR #363 that
#      made a self-authored thread look like independent coverage.
#
#   2. A review comment's `commit_id` is re-anchored to the CURRENT
#      head whenever the file it points at has not changed. A finding
#      written against an old commit therefore DISPLAYS as though it
#      were evaluated against code that did not exist when it was
#      written. `original_commit_id` is the honest field.
#
# So: filter by AUTHOR, and compare the newest independent review's
# commit against headRefOid. Everything else is noise.
#
# WAITING (--wait). Automated review fires on PR **open** and on
# explicit request — never on a push. So there are two very different
# "no review yet" states, and only one of them is worth waiting through:
#
#   * never reviewed          — a review is coming; wait for it.
#   * reviewed, then pushed   — nothing is coming, ever, until someone
#                               asks. Waiting cannot help, and a timeout
#                               here would report "no review" as though
#                               the bot were merely slow.
#
# The second exits 5 IMMEDIATELY with the cause named, rather than
# burning the timeout. That distinction is the reason to wait with this
# instead of sleeping. Same shape as wait-merged.sh: run it detached and
# the exit is the callback.
#
# Usage:
#   tools/gh/pr-review-status.sh <pr-number> [owner/repo] [options]
#
# Options:
#   --wait <duration>      poll until the head is reviewed (default 0 =
#                          answer once and exit, the original behaviour)
#   --interval <duration>  between polls (default 30, as wait-merged.sh;
#                          the API is rate limited and humans are slow)
#
# Durations take an optional unit — 90, 90s, 10m, 2h. A bare number is
# SECONDS, so anything written before units existed still means what it
# meant. wait-merged.sh accepts exactly the same forms.
#   -q, --quiet            no progress lines on stderr
#   -h, --help             this text
#
# Exit codes:
#   0  the current head has at least one independent review
#   1  it does not — nothing yet, or --wait expired still waiting
#   2  invocation problem (no gh, not authenticated, unknown PR)
#   5  no review is COMING — the head advanced past the newest
#      independent review and no review is requested. Ask for one;
#      waiting is futile. Distinguished from 1 the way wait-merged.sh
#      distinguishes 6 from 4: 1 means "not yet", 5 names the cause.
# <<< help

set -uo pipefail

PR=""
REPO=""
WAIT=0
INTERVAL=30
QUIET=0

die() { echo "pr-review-status: $*" >&2; exit 2; }

need_operand() {
    [[ $# -ge 2 ]] || die "$1 needs a value (try --help)."
}

# Durations take an optional unit: 90, 90s, 10m, 2h. A BARE NUMBER IS
# SECONDS — which is what every existing invocation already meant, so
# nothing changes for a caller that passed one.
#
# wait-merged.sh carries an identical copy. That is deliberate: these are
# standalone scripts with no shared library, and a divergence in what
# they accept is exactly the confusion the units were added to remove.
# Both suites assert the same table, so a drift fails a test.
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
        # script would sail on with an empty value.
        --wait)
            need_operand "$@"; WAIT="$(as_seconds "$1" "$2")" || exit 2; shift 2 ;;
        --interval)
            need_operand "$@"; INTERVAL="$(as_seconds "$1" "$2")" || exit 2
            want_positive "$1" "$INTERVAL" "$2"; shift 2 ;;
        -q|--quiet) QUIET=1; shift ;;
        -h|--help)
            sed -n '/^# >>> help$/,/^# <<< help$/p' "$0" \
                | sed 's/^# \{0,1\}//; 1d; $d'
            exit 0 ;;
        -*) die "unknown option '$1' (try --help)" ;;
        *)
            if   [[ -z "$PR"   ]]; then PR="$1"
            elif [[ -z "$REPO" ]]; then REPO="$1"
            else die "unexpected argument '$1' (try --help)"
            fi
            shift ;;
    esac
done

[[ -n "$PR" ]] || die "a PR number is required (try --help)"

command -v gh >/dev/null 2>&1 || die "gh is required but not installed."

if [[ -z "$REPO" ]]; then
    REPO="$(gh repo view --json nameWithOwner --jq .nameWithOwner 2>/dev/null)" || \
        die "could not determine the repository; pass owner/repo."
fi

note() { (( QUIET )) || echo "pr-review-status: $*" >&2; }

# ── One probe ────────────────────────────────────────────────────────
#
# Sets the globals the loop branches on. Kept separate from rendering so
# a poll costs two API calls, not the whole report: the thread and check
# queries below are for the human reading the final answer, and running
# them every 30 seconds would be rude to the rate limiter for output
# nobody sees.
probe_ok=0
probe() {
    local meta reviews
    meta="$(gh pr view "$PR" --repo "$REPO" \
        --json state,mergeStateStatus,headRefOid,author,isDraft,reviewRequests \
        2>/dev/null)" || return 1

    head="$(jq -r '.headRefOid'          <<<"$meta")"
    state="$(jq -r '.state'              <<<"$meta")"
    mergest="$(jq -r '.mergeStateStatus'  <<<"$meta")"
    author="$(jq -r '.author.login'      <<<"$meta")"
    requested="$(jq '.reviewRequests | length' <<<"$meta")"

    # PAGINATED, and a failure here is UNREADABLE — never empty. This
    # script's entire job is answering "was this really reviewed", so
    # converting a rate limit, a permission gap, or a transient 5xx into
    # `[]` would produce the confident wrong answer "no reviews" and let
    # a caller conclude a reviewed PR was never looked at. Returning 1
    # feeds the consecutive-failure counter instead, which is what the
    # exit-2 contract already promises for an unreadable PR.
    local reviews_raw
    reviews_raw="$(gh api --paginate "repos/$REPO/pulls/$PR/reviews" 2>/dev/null)" || return 1
    reviews="$(jq -s 'add // []' <<<"$reviews_raw" 2>/dev/null)" || return 1

    # Independent = not authored by the PR author. The empty-body test is
    # NOT used to classify: a genuine reviewer may leave an empty-bodied
    # review carrying only inline comments. Authorship is the honest axis.
    independent="$(jq --arg a "$author" '[.[] | select(.user.login != $a)]' <<<"$reviews")"
    self="$(jq --arg a "$author" '[.[] | select(.user.login == $a)]' <<<"$reviews")"

    ind_count="$(jq 'length' <<<"$independent")"
    self_count="$(jq 'length' <<<"$self")"

    head_reviewed=no
    newest_ind_commit=""
    if (( ind_count > 0 )); then
        newest_ind_commit="$(jq -r 'sort_by(.submitted_at) | last | .commit_id' <<<"$independent")"
        # COVERAGE IS "ANY review targets head", NOT "the newest one
        # does". A review created before the last push but submitted
        # after a fresh one is newer by timestamp and older by commit, so
        # selecting by recency lets a stale review mask a real one — and
        # this script would then report the head unreviewed, and exit 5
        # claiming no review is coming, while the review it needed was
        # already sitting there. The newest is kept for REPORTING only.
        if [[ "$(jq -r --arg h "$head" 'any(.[]; .commit_id == $h)' <<<"$independent")" == "true" ]]; then
            head_reviewed=yes
        fi
    fi
    # Only a COMPLETE probe counts. A probe that read the PR metadata and
    # then failed on reviews leaves head set but the review globals unset,
    # and the fall-out path below must not mistake that for readable data
    # and render a zero-coverage report from it.
    probe_ok=1
    return 0
}

# Nothing is coming. Requires ALL of: a previous independent review (so
# this is not a fresh PR, where review fires on open), a head that has
# moved past it, and no pending request. Any one of those missing and
# waiting is still the right move.
no_review_coming() {
    [[ "$head_reviewed" == "no" ]] \
        && (( ind_count > 0 )) \
        && (( requested == 0 ))
}

# ── The full report, rendered once ───────────────────────────────────
render() {
    local threads unresolved checks_pass checks_other

    # Unresolved threads no longer gate the merge (the ruleset dropped
    # required_review_thread_resolution on 2026-08-06), so they belong in
    # this glance more than ever — nothing else will raise them.
    threads="$(gh api graphql -f query='
      query($owner:String!,$name:String!,$pr:Int!){
        repository(owner:$owner,name:$name){
          pullRequest(number:$pr){
            reviewThreads(first:100){nodes{isResolved isOutdated path}}}}}' \
      -F owner="${REPO%%/*}" -F name="${REPO##*/}" -F pr="$PR" \
      --jq '[.data.repository.pullRequest.reviewThreads.nodes[]|select(.isResolved==false)]' 2>/dev/null)" \
      || threads='[]'
    unresolved="$(jq 'length' <<<"$threads")"

    checks_pass="$(gh pr checks "$PR" --repo "$REPO" 2>/dev/null | grep -cE '\spass\s' || true)"
    checks_other="$(gh pr checks "$PR" --repo "$REPO" 2>/dev/null | grep -vcE '\spass\s' || true)"

    printf 'PR #%s  state=%s  mergeState=%s  head=%s\n' \
        "$PR" "$state" "$mergest" "${head:0:8}"
    printf '  independent reviews : %s' "$ind_count"
    if (( ind_count > 0 )); then
        printf '   (newest against %s)' "${newest_ind_commit:0:8}"
    fi
    printf '\n'
    if (( ind_count > 0 )); then
        jq -r '.[] | "      - \(.user.login)  \(.state)  commit=\(.commit_id[0:8])  \(.submitted_at)"' \
            <<<"$independent"
    fi
    printf '  self reviews        : %s   (thread replies etc. — not coverage)\n' "$self_count"
    printf '  review requested?   : %s\n' "$( (( requested > 0 )) && echo yes || echo no )"
    printf '  head reviewed?      : %s\n' "$head_reviewed"
    if [[ "$head_reviewed" == "no" && "$ind_count" -gt 0 ]]; then
        printf '                        ^ reviewed, but an EARLIER commit. Pushes do not\n'
        printf '                          re-trigger automated review — request it explicitly.\n'
    fi
    printf '  unresolved threads  : %s\n' "$unresolved"
    if (( unresolved > 0 )); then
        jq -r '.[] | "      - \(.path)  outdated=\(.isOutdated)"' <<<"$threads"
    fi
    printf '  checks              : %s pass, %s other\n' "$checks_pass" "$checks_other"
}

# ── Poll ─────────────────────────────────────────────────────────────
#
# A single failed lookup is a blip, not a verdict; only a run of them
# means the PR is genuinely unreadable. Same tolerance as wait-merged.sh.
consecutive_failures=0
deadline=$(( SECONDS + WAIT ))

while :; do
    if probe; then
        consecutive_failures=0

        if [[ "$head_reviewed" == "yes" ]]; then
            render; exit 0
        fi

        if no_review_coming; then
            note "head has advanced past the newest review and none is requested"
            render
            printf 'PR #%s NO REVIEW COMING — request one; waiting cannot help (exit 5)\n' "$PR"
            exit 5
        fi

        # A PR closed without merging will receive nothing further. A
        # MERGED one still can, and routinely does here — that is the
        # whole reason the post-merge sweep exists — so it keeps waiting.
        if [[ "$state" == "CLOSED" ]]; then
            render; exit 1
        fi
    else
        consecutive_failures=$(( consecutive_failures + 1 ))
        (( consecutive_failures >= 3 )) && die "could not read PR #$PR in $REPO."
        note "could not read PR #$PR (attempt $consecutive_failures); retrying"
    fi

    (( WAIT > 0 )) || break
    (( SECONDS + INTERVAL <= deadline )) || break
    note "no independent review of ${head:0:8} yet; next check in ${INTERVAL}s"
    sleep "$INTERVAL"
done

# Fell out: either one-shot, or the wait expired with nothing. Requires a
# probe that completed — `head` alone is not enough, because it is set
# before the reviews lookup that may be the thing that failed.
if (( probe_ok == 0 )) || [[ -z "${head:-}" ]]; then
    die "could not read PR #$PR in $REPO."
fi
render
exit 1
