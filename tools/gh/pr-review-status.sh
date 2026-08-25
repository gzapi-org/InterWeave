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
# So: filter by AUTHOR, and ask whether ANY independent review targets
# headRefOid. Recency is the wrong axis — a review created before the
# last push but submitted after a fresh one is newer by timestamp and
# older by commit, so selecting the newest lets a stale review mask real
# coverage and produces a false "no review is coming". The newest is kept
# for reporting only. Everything else is noise.
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
#   --automated-only       only a RECOGNISED reviewer's review counts as
#                          coverage. Required by CLAUDE.md §9 before
#                          arming --auto on a security-boundary change.
#
# WHY --automated-only EXISTS, and why the bare command does not satisfy
# §9. "Not the PR author" is the right test for INDEPENDENCE and the
# wrong test for a gate that is waiting on one named reviewer. This
# repository is PUBLIC, so any GitHub user can submit a review object on
# an open PR — and one drive-by review carrying the current head is
# enough to make this command exit 0 and a session arm the merge the
# gate exists to hold open. That is the same hole the verdict-comment
# path already closes with an allow-list, for the reason stated there: a
# negation is what lets everybody in.
#
# Under the flag the allow-list narrows THREE things together, because a
# mode that narrows coverage and leaves the freshness terms reading a
# different population answers a question nobody asked:
#
#   * which review objects cover the head;
#   * which pending review REQUEST suppresses the exit-5 verdict, so a
#     human reviewer being slow does not mask a bot review that is not
#     coming;
#   * whether any prior review exists at all, the "this is not a fresh
#     PR" term.
#
# It is off by default because the unnarrowed question — was this PR
# reviewed, by anyone — is a real question and a human review is a real
# answer to it. Without the flag, coverage that rests only on an
# unrecognised account is reported as such rather than passed off.
#
# Durations take an optional unit — 90, 90s, 10m, 2h. A bare number is
# SECONDS, so anything written before units existed still means what it
# meant. wait-merged.sh accepts exactly the same forms.
#   -q, --quiet            no progress lines on stderr
#   -h, --help             this text
#
# A CLEAN REVIEW IS NOT A REVIEW OBJECT. When the automated reviewer
# finds nothing it posts an ordinary issue COMMENT — "Didn't find any
# major issues" — and creates no review. Counting only review objects
# therefore reported `head reviewed? no` on exactly the PRs that passed,
# and then exit 5, "no review is coming", about a review that had
# already happened and succeeded. CLAUDE.md §9 tells a session to wait
# for the review before arming a security-boundary change, so that
# false negative turned the happy path into an indefinite wait.
#
# Those comments name what they looked at — `**Reviewed commit:** <sha>`
# — so this reads the sha rather than inferring coverage from a
# timestamp. Inferring would be the confident wrong answer this script
# exists to avoid: a verdict comment can arrive after a push and still
# describe the commit before it.
#
# Exit codes:
#   0  the current head has at least one independent review (with
#      --automated-only: one from a recognised reviewer)
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
AUTOMATED_ONLY=0

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
        --automated-only) AUTOMATED_ONLY=1; shift ;;
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
# Accounts whose verdict comment counts as a review.
#
# A LIST, because the alternative is a negation and a negation is what
# lets anybody in. Bots appear with a `[bot]` suffix in some API
# surfaces and without it in others, so both spellings are listed rather
# than normalised -- a normalisation that silently stopped matching
# would fail open.
#
# Override with INTERWEAVE_VERDICT_AUTHORS (a JSON array) where a
# repository uses a different reviewer.
VERDICT_AUTHORS="${INTERWEAVE_VERDICT_AUTHORS:-[\"chatgpt-codex-connector\",\"chatgpt-codex-connector[bot]\"]}"

probe_ok=0
verdicts='[]'
verdict_count=0
request_at=""
request_by=""
request_pending=no
# Set before the first probe runs. The progress line below prints
# `${head:0:8}`, and under `set -u` an unset `head` is a fatal error --
# so a run whose FIRST probe failed died mid-loop with "unbound
# variable" and exit 1 instead of retrying and reporting exit 2, the
# unreadable-PR contract. Reachable whenever --wait is long enough to
# reach the second poll, which is every real invocation.
head=""
probe() {
    local meta reviews

    # RESET, not just set on success. `probe_ok` is what the fall-out
    # path below uses to decide whether it holds readable data, and a
    # probe that reads the PR metadata and then fails on reviews leaves
    # a NEW head beside the PREVIOUS poll's review counts. Left latched
    # from an earlier success, that renders as a current report of a
    # state that was never observed.
    probe_ok=0
    meta="$(gh pr view "$PR" --repo "$REPO" \
        --json state,mergeStateStatus,headRefOid,author,isDraft,reviewRequests \
        2>/dev/null)" || return 1

    head="$(jq -r '.headRefOid'          <<<"$meta")"
    state="$(jq -r '.state'              <<<"$meta")"
    mergest="$(jq -r '.mergeStateStatus'  <<<"$meta")"
    author="$(jq -r '.author.login'      <<<"$meta")"

    # NARROWED BY THE MODE, exactly as `qualifying` is below.
    #
    # `requested` suppresses the exit-5 verdict, so under
    # --automated-only a pending HUMAN reviewer would keep this command
    # waiting and then reporting 1 about a bot review that is never
    # coming. A mode that narrows coverage has to narrow whatever
    # answers "is more coverage on its way", or the two terms are
    # reading different populations.
    if (( AUTOMATED_ONLY )); then
        requested="$(jq --argjson allowed "$VERDICT_AUTHORS" \
            '[.reviewRequests[]? | select((.login // .slug // "") as $l | $allowed | index($l))] | length' \
            <<<"$meta")"
    else
        requested="$(jq '.reviewRequests | length' <<<"$meta")"
    fi

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

    # WHICH REVIEWS COUNT AS COVERAGE, which is a different question from
    # which are independent.
    #
    # THIS REPOSITORY IS PUBLIC: any GitHub user may submit a review
    # object on an open PR, so "not the PR author" admits a stranger.
    # Under --automated-only a review only covers the head if it came
    # from the same allow-list the verdict comments use, and for the same
    # reason given there -- a negation is what lets everybody in.
    #
    # `recognised` is computed in BOTH modes, because the default mode
    # still has to be able to say that the coverage it is reporting rests
    # on an account nobody recognises.
    recognised="$(jq --argjson allowed "$VERDICT_AUTHORS" \
        '[.[] | select(.user.login as $l | $allowed | index($l))]' <<<"$independent")"
    if (( AUTOMATED_ONLY )); then
        qualifying="$recognised"
    else
        qualifying="$independent"
    fi

    ind_count="$(jq 'length' <<<"$independent")"
    qual_count="$(jq 'length' <<<"$qualifying")"
    self_count="$(jq 'length' <<<"$self")"

    # VERDICT COMMENTS. Unreadable rather than empty, for the same
    # reason the reviews call is: a swallowed failure here would report
    # a clean review as no review.
    local comments_raw comments
    comments_raw="$(gh api --paginate "repos/$REPO/issues/$PR/comments" 2>/dev/null)" || return 1
    comments="$(jq -s 'add // []' <<<"$comments_raw" 2>/dev/null)" || return 1

    # A comment from a RECOGNISED REVIEWER that names the commit it
    # reviewed. The sha is abbreviated in the body, so match on prefix
    # in BOTH directions -- neither string is reliably the longer one.
    #
    # "not the PR author" is the right filter for a review OBJECT,
    # because GitHub only lets a reviewer create one. It is the wrong
    # filter for a comment: on a public repository anyone may leave one,
    # so accepting any non-author comment containing the verdict phrase
    # lets a third party mark a head reviewed by typing it. That would
    # satisfy the §9 prerequisite for auto-merging a security-boundary
    # change without the reviewer ever having run -- a spoof of the
    # exact gate this function was added to make usable.
    #
    # So the account is checked against a list, not against a negation.
    verdicts="$(jq --arg a "$author" --argjson allowed "$VERDICT_AUTHORS" '
        [ .[]
          | select(.user.login as $l | $allowed | index($l))
          | select(.user.login != $a)
          | (.body // "") as $b
          | ($b | capture("Reviewed commit:[^`]*`(?<sha>[0-9a-f]{7,40})`"; "i") // empty) as $m
          | {login: .user.login, at: .created_at, sha: $m.sha}
        ]' <<<"$comments" 2>/dev/null)" || verdicts='[]'
    verdict_count="$(jq 'length' <<<"$verdicts")"

    # A PENDING ASK, read from the same comment stream.
    #
    # `@codex review` is how a review is requested here, and it is NOT a
    # GitHub review *request* -- `reviewRequests` stays empty -- so
    # `requested == 0` held while one was genuinely in flight and the
    # stale-review branch fired NO REVIEW COMING seconds after the ask.
    # That broke the exact sequence §9 prescribes for a security-boundary
    # change: request a review, then wait for it. §9 documented the wart
    # instead of fixing it, which left its own instruction unusable.
    #
    # NO ALLOW-LIST here, and the asymmetry from the verdict filter is
    # deliberate. Two reasons, and the second is the stronger one:
    #
    #   * this signal can only turn 5 ("nothing is coming") into 1 ("not
    #     yet"). It never marks a head reviewed, so a forged ask cannot
    #     arm a merge -- the worst it does is make the tool wait out the
    #     --wait the caller chose, and then exit 1.
    #   * there is no correct list to check it against. The ask comes
    #     from whoever is shepherding the PR, not from the reviewer, so
    #     VERDICT_AUTHORS is the wrong set; "the PR author" would reject
    #     a legitimate ask from anyone else. A filter with no right
    #     answer fails closed on real asks, which is the failure that
    #     costs something.
    #
    # The report names WHO asked, so a stray ask is visible rather than
    # merely effective.
    local asks
    asks="$(jq '[ .[] | select((.body // "") | test("@codex[[:space:]]+review"; "i"))
                 | {at: .created_at, by: .user.login} ] | sort_by(.at)' \
             <<<"$comments" 2>/dev/null)" || asks='[]'
    request_at="$(jq -r 'last.at // ""' <<<"$asks")"
    request_by="$(jq -r 'last.by // ""' <<<"$asks")"

    head_reviewed=no
    recognised_covers=no
    newest_ind_commit=""

    # Either kind of evidence counts, and a verdict comment is checked
    # even when there are no review objects at all: a PR whose only
    # review was clean has none.
    if [[ "$(jq -r --arg h "$head" \
          'any(.[]; (.sha as $s | ($h | startswith($s)) or ($s | startswith($h))))' \
          <<<"$verdicts")" == "true" ]]; then
        head_reviewed=yes
        recognised_covers=yes
    fi

    # Tracked in BOTH modes. Under --automated-only this is the same
    # answer as `head_reviewed`; without it, it is what lets the report
    # disclose that the only thing covering this head is a review object
    # from an account the allow-list does not know.
    if [[ "$(jq -r --arg h "$head" 'any(.[]; .commit_id == $h)' <<<"$recognised")" == "true" ]]; then
        recognised_covers=yes
    fi

    if (( qual_count > 0 )); then
        newest_ind_commit="$(jq -r 'sort_by(.submitted_at) | last | .commit_id' <<<"$qualifying")"
        # COVERAGE IS "ANY review targets head", NOT "the newest one
        # does". A review created before the last push but submitted
        # after a fresh one is newer by timestamp and older by commit, so
        # selecting by recency lets a stale review mask a real one — and
        # this script would then report the head unreviewed, and exit 5
        # claiming no review is coming, while the review it needed was
        # already sitting there. The newest is kept for REPORTING only.
        if [[ "$(jq -r --arg h "$head" 'any(.[]; .commit_id == $h)' <<<"$qualifying")" == "true" ]]; then
            head_reviewed=yes
        fi
    fi
    # IS THAT ASK STILL OUTSTANDING? Timestamps alone get this wrong in
    # both directions.
    #
    # Comparing the ask against the newest REVIEW fails when a review of
    # an earlier commit lands after a newer head was pushed and asked
    # about: it reads as the answer, and exit 5 fires while the real
    # review is still in flight. That is recency mistaken for coverage,
    # the same confusion the head-reviewed test already avoids.
    #
    # Requiring a HEAD-MATCHING review to answer the ask -- the obvious
    # repair -- breaks the other direction: asked, reviewed, then pushed
    # again without asking. No review can ever match the new head, so the
    # ask stays pending forever and exit 5 could never fire, which is the
    # one thing it exists to say.
    #
    # The HEAD's OWN COMMIT DATE separates them cleanly: an ask made
    # after this head existed is an ask about this head. Fetched only
    # when there is an ask, so the common path keeps its three calls.
    request_pending=no
    if [[ -n "$request_at" ]]; then
        local head_born
        head_born="$(gh api "repos/$REPO/commits/$head" \
            --jq '.commit.committer.date' 2>/dev/null)" || head_born=""
        # Unreadable falls back to PENDING, which costs a longer wait and
        # never a false "nothing is coming".
        if [[ -z "$head_born" || "$request_at" > "$head_born" ]]; then
            request_pending=yes
        fi
        # The review it asked for answers it. No exit path reaches here
        # with a covered head -- head_reviewed wins first -- so this
        # decides nothing; it stops the REPORT printing "nothing has
        # answered it yet" beside "head reviewed? yes", which is simply
        # false.
        [[ "$head_reviewed" == "yes" ]] && request_pending=no
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
    # A prior VERDICT comment is equally evidence that this PR is one
    # the reviewer answers, so it satisfies the "not a fresh PR" term
    # just as a review object does.
    #
    # `qual_count`, not `ind_count`: under --automated-only a stranger's
    # review is not evidence that the recognised reviewer has already had
    # its turn, and counting it would report "nothing is coming" about a
    # first review still on its way.
    [[ "$head_reviewed" == "no" ]] \
        && (( qual_count + verdict_count > 0 )) \
        && (( requested == 0 )) \
        && [[ "$request_pending" != "yes" ]]
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
    if (( qual_count > 0 )); then
        printf '   (newest against %s)' "${newest_ind_commit:0:8}"
    fi
    if (( AUTOMATED_ONLY )) && (( ind_count > qual_count )); then
        printf '   [%s not the recognised reviewer — NOT counted]' \
            "$(( ind_count - qual_count ))"
    fi
    printf '\n'
    if (( ind_count > 0 )); then
        jq -r '.[] | "      - \(.user.login)  \(.state)  commit=\(.commit_id[0:8])  \(.submitted_at)"' \
            <<<"$independent"
    fi
    printf '  verdict comments    : %s' "$verdict_count"
    if (( verdict_count > 0 )); then
        printf '   (a clean review leaves no review object)'
    fi
    printf '\n'
    if (( verdict_count > 0 )); then
        jq -r '.[] | "      - \(.login)  commit=\(.sha[0:8])  \(.at)"' <<<"$verdicts"
    fi
    printf '  self reviews        : %s   (thread replies etc. — not coverage)\n' "$self_count"
    printf '  review requested?   : %s\n' "$( (( requested > 0 )) && echo yes || echo no )"
    if [[ -n "$request_at" ]]; then
        # WHO asked is reported, not just when. `@codex review` is not
        # allow-listed, so on a public repository the login is what
        # separates the ask this session made from one it did not.
        printf '  @codex review asked : %s  by %s%s\n' "$request_at" "$request_by" \
            "$( [[ "$request_pending" == "yes" ]] \
                  && echo '   (in flight — nothing has answered it yet)' \
                  || echo '   (already answered)' )"
    fi
    printf '  head reviewed?      : %s%s\n' "$head_reviewed" \
        "$( (( AUTOMATED_ONLY )) && echo '   (--automated-only: the recognised reviewer, not just anyone)' )"

    # THE CALLER WHO FORGOT THE FLAG still gets told. On a public
    # repository the dangerous shape is coverage that rests entirely on
    # an account nobody recognises, and a session following §9 reads the
    # exit code -- so the exposure has to be visible in the default mode
    # too, not only in the mode that already excludes it.
    if [[ "$head_reviewed" == "yes" && "$recognised_covers" == "no" ]]; then
        printf '                        ^ carried ONLY by an unrecognised account. This repo\n'
        printf '                          is public: anyone may review. CLAUDE.md §9 wants\n'
        printf '                          --automated-only before arming a security change.\n'
    fi
    if [[ "$head_reviewed" == "no" && "$qual_count" -gt 0 ]]; then
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

        # TERMINAL STATE FIRST. A PR closed without merging will receive
        # nothing further. A MERGED one still can, and routinely does
        # here — that is the whole reason the post-merge sweep exists —
        # so it keeps waiting.
        if [[ "$state" == "CLOSED" ]]; then
            render; exit 1
        fi

        # ...which is why exit 5 is restricted to an OPEN PR. Run before
        # the state check, `no_review_coming` fires on a merged PR whose
        # only review predates the last push and tells the caller to
        # request one — contradicting the merged-PR exception directly
        # above it, and sending them away from the sweep that was about
        # to review it.
        if [[ "$state" == "OPEN" ]] && no_review_coming; then
            note "head has advanced past the newest review and none is requested"
            render
            printf 'PR #%s NO REVIEW COMING — request one; waiting cannot help (exit 5)\n' "$PR"
            exit 5
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
