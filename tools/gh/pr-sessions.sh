#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/gh/pr-sessions.sh
#
# >>> help
# Which SESSION owns which PR, newest first.
#
# Every PR in this repo carries the same GitHub author, because every
# session pushes with the same credentials. `gh pr list` therefore shows
# one name against all of them and answers nothing about who is doing
# what. The session identity lives in the BRANCH NAME instead — the
# repo-root CLAUDE.md mandates
#
#     <hostname -s>/<clone-dir-basename>/<type>/<short-desc>
#
# so the first two segments are the session, and everything after is the
# work. This script reads that, and marks the rows belonging to THIS
# clone so "mine vs theirs" is visible at a glance — which is what the
# stay-in-your-own-lane rules turn on: never push to, rebase, delete or
# answer reviews on another session's branch.
#
# DEFAULT SCOPE: run inside a clone, and it shows THAT clone's PRs.
# Asking "which PRs are mine" from inside a working tree is the common
# case, and the clone you are standing in already answers it — so that
# is the default rather than something to remember a flag for. Pass
# /all to see every session.
#
# (There is no unscoped-outside-a-repo case to describe: `gh pr list`
# needs a repo context and fails first, so the script never gets that
# far. The empty-ME branch below is belt-and-braces for a future
# --repo flag, not a path you can reach today.)
#
# Usage:
#   tools/gh/pr-sessions.sh                 # THIS clone's PRs (default)
#   tools/gh/pr-sessions.sh /all            # every session
#   tools/gh/pr-sessions.sh -n 50           # last 50 rows
#   tools/gh/pr-sessions.sh /OPEN           # open PRs only
#   tools/gh/pr-sessions.sh /MERGED         # merged only  (/CLOSED too)
#   tools/gh/pr-sessions.sh --session interweave-clone2
#   tools/gh/pr-sessions.sh /all --by-session   # grouped, all sessions
#   tools/gh/pr-sessions.sh /lastItem:50    # pool = 50 most recent PRs
#   tools/gh/pr-sessions.sh /lastDate:2d    # pool = updated in the last 2 days
#   tools/gh/pr-sessions.sh /lastDate:6h /unresolved   # ...then filter
#   tools/gh/pr-sessions.sh /unresolved     # only PRs with an open thread
#   tools/gh/pr-sessions.sh /all /unresolved # ...across every session
#
# State is chosen at FETCH time, so like /lastItem and /lastDate it
# narrows the pool before scope, /unresolved and -n ever see it.
#   tools/gh/pr-sessions.sh --no-threads    # skip the review-thread lookup
#
# /lastItem and /lastDate narrow the POOL, and are applied BEFORE
# everything else — session scope, /unresolved and -n all operate on
# what they leave behind. That is what separates them from -n: -n trims
# the printed page, these decide what was ever considered.
#
# The THR column counts UNRESOLVED review threads. These no longer gate
# a merge (required_review_thread_resolution went off 2026-08-06), which
# is precisely why the column matters: a PR can merge with findings
# outstanding, so this count is the only place they surface. A
# trailing "!" means the last word in at least one of them is NOT the
# PR author's, i.e. somebody is waiting on a reply. "2" without the
# bang means you answered and simply have not resolved the threads.
#
# Exit codes:
#   0  listed (even if the result is empty)
#   2  invocation problem (no gh/jq, not authenticated, bad flag)
# <<< help

set -uo pipefail

LIMIT=20
STATE=all
STATE_SET=""      # which flag chose STATE, so a conflict can be named
FILTER=""
GROUPED=0
THREADS=1
UNRESOLVED_ONLY=0
# _SET flags rather than an empty-string sentinel: `/lastItem:` and
# `/lastDate:` PASS an empty value, and treating that as "unset" made a
# malformed flag render as no filter at all — the whole list, looking
# like a successful narrow. Passed-but-empty must be an error.
LAST_ITEM=""; LAST_ITEM_SET=0     # /lastItem:N  — pool = N most recent PRs
LAST_DATE=""; LAST_DATE_SET=0     # /lastDate:Nd — pool = updated within window
CUTOFF=""                         # LAST_DATE resolved to an ISO instant
SCOPE_EXPLICIT=0    # did the caller choose a scope, overriding the default?

# gh takes ONE --state. Two conflicting flags would otherwise resolve
# to whichever came last, quietly showing a different set than asked.
set_state() {
    if [[ -n "$STATE_SET" && "$STATE" != "$1" ]]; then
        echo "pr-sessions: $STATE_SET and $2 conflict — pick one state." >&2
        exit 2
    fi
    STATE="$1"; STATE_SET="$2"
}

# A flag that takes a value must HAVE one. Without this, `-n` as the
# final argument left `shift 2` with nothing to consume: shift fails,
# consumes nothing, and — `set -e` being deliberately off — the loop
# re-reads the same argument forever. A hung script is a worse failure
# than a rejected one, and it looks like a slow network call.
need_operand() {
    [[ $# -ge 2 ]] || {
        echo "pr-sessions: $1 needs a value (try --help)." >&2; exit 2; }
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        -n|--limit)   need_operand "$@"; LIMIT="$2"; shift 2 ;;
        /OPEN|/open|--open)       set_state open   "$1"; shift ;;
        /MERGED|/merged|--merged) set_state merged "$1"; shift ;;
        /CLOSED|/closed|--closed) set_state closed "$1"; shift ;;
        /all|--all)   FILTER=""; SCOPE_EXPLICIT=1; shift ;;
        --mine)       FILTER="__MINE__"; SCOPE_EXPLICIT=1; shift ;;
        --session)    need_operand "$@"; FILTER="$2"; SCOPE_EXPLICIT=1; shift 2 ;;
        --by-session) GROUPED=1; shift ;;
        --no-threads) THREADS=0; shift ;;
        /unresolved|--unresolved) UNRESOLVED_ONLY=1; shift ;;
        /lastItem:*|--lastItem:*)  LAST_ITEM="${1#*:}"; LAST_ITEM_SET=1; shift ;;
        /lastDate:*|--lastDate:*)  LAST_DATE="${1#*:}"; LAST_DATE_SET=1; shift ;;
        # Delimited by markers, not line numbers. `sed -n '3,36p'` meant
        # every flag documented below line 36 — /lastItem, /lastDate,
        # /unresolved, --by-session, --no-threads — was invisible to the
        # --help the script's own error messages tell you to run. A help
        # range pinned to line numbers goes stale the first time anything
        # above it grows, and nothing complains.
        -h|--help)
            sed -n '/^# >>> help$/,/^# <<< help$/p' "$0" \
                | sed '1d;$d' | sed 's/^# \{0,1\}//'
            exit 0 ;;
        *)            echo "pr-sessions: unknown option '$1' (try --help)" >&2; exit 2 ;;
    esac
done

# -n reaches `$(( LIMIT * 20 ))`, and bash arithmetic re-evaluates the
# CONTENTS of a variable as an expression — including command
# substitution inside an array subscript. So `-n 'x[$(rm -rf …)]'` is
# not a bad number, it is code execution. Validate before any
# arithmetic, not at the point of use.
if [[ ! "$LIMIT" =~ ^[1-9][0-9]*$ ]]; then
    echo "pr-sessions: -n/--limit needs a positive integer, got '$LIMIT'." >&2
    exit 2
fi

# Validated whenever the flag was PASSED, not merely when non-empty.
if [[ "$LAST_ITEM_SET" -eq 1 ]]; then
    [[ "$LAST_ITEM" =~ ^[1-9][0-9]*$ ]] || {
        echo "pr-sessions: /lastItem needs a positive integer, got '$LAST_ITEM'." >&2; exit 2; }
fi

if [[ "$LAST_DATE_SET" -eq 1 ]]; then
    # Nd / Nh / Nm. A bare number is REFUSED rather than assumed to be
    # days: guessing the unit on a time filter silently changes which
    # PRs you are looking at.
    if [[ "$LAST_DATE" =~ ^([1-9][0-9]*)([dhm])$ ]]; then
        n="${BASH_REMATCH[1]}"
        case "${BASH_REMATCH[2]}" in
            d) span="$n days ago" ;;
            h) span="$n hours ago" ;;
            m) span="$n minutes ago" ;;
        esac
        CUTOFF="$(date -u -d "$span" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null)" || {
            echo "pr-sessions: could not compute a cutoff for '$LAST_DATE'." >&2; exit 2; }
    else
        echo "pr-sessions: /lastDate needs <N>d, <N>h or <N>m — got '$LAST_DATE'." >&2
        exit 2
    fi
fi

if [[ "$UNRESOLVED_ONLY" -eq 1 && "$THREADS" -eq 0 ]]; then
    echo "pr-sessions: /unresolved needs the thread lookup — drop --no-threads." >&2
    exit 2
fi

for bin in gh jq; do
    command -v "$bin" >/dev/null 2>&1 || {
        echo "pr-sessions: $bin is required but not installed." >&2; exit 2; }
done

# This clone's session, derived the same way the branch prefix is built.
ME=""
if root="$(git rev-parse --show-toplevel 2>/dev/null)"; then
    ME="$(hostname -s)/$(basename "$root")"
fi

# Inside a clone with no explicit scope: show that clone's PRs. If ME
# cannot be resolved there is nothing to infer, so fall through to
# everything — unreachable today (gh needs a repo and fails earlier),
# kept so the default cannot silently become "someone else's session"
# if a --repo flag is ever added.
DEFAULTED_TO_MINE=0
if [[ "$SCOPE_EXPLICIT" -eq 0 && -n "$ME" ]]; then
    FILTER="__MINE__"
    DEFAULTED_TO_MINE=1
fi
[[ -z "$ME" ]] && ME="unknown/unknown"

# -n bounds the ROWS SHOWN, not how far back we look. With a filter the
# two differ sharply: `--mine -n 8` fetching only the newest 8 PRs
# returned NOTHING here, because all 8 belonged to parallel sessions —
# an empty list that reads as "you have no PRs" rather than "your PRs
# are older than the window". So widen the fetch and trim after
# filtering.
#
# 500 bounds the fetch this script INFERS for you. It is a guess about
# how far back to look, so capping it is free.
DERIVED_FETCH_CAP=500
FETCH="$LIMIT"
if [[ -n "$FILTER" || "$UNRESOLVED_ONLY" -eq 1 || -n "$CUTOFF" || -n "$LAST_ITEM" ]]; then
    FETCH=$(( LIMIT * 20 )); (( FETCH < 200 )) && FETCH=200
    (( FETCH > DERIVED_FETCH_CAP )) && FETCH=$DERIVED_FETCH_CAP
fi
# An EXPLICIT /lastItem:N is not a guess — it is the pool the caller
# asked for, and it is honoured whole. The cap used to apply here too,
# so /lastItem:600 quietly built a 600-row pool out of 500 rows and
# then filtered it, dropping matches without a word. `gh pr list
# --limit` paginates, so there is nothing to clamp for.
if [[ -n "$LAST_ITEM" ]] && (( LAST_ITEM > FETCH )); then
    FETCH="$LAST_ITEM"
fi

# /unresolved filters on data that only exists AFTER the thread lookup,
# so the candidate set has to be wider than the page — otherwise a PR
# with an open thread just past row -n would be invisible, which is the
# same window trap `--mine -n 8` fell into. Capped so the aliased
# GraphQL query stays one sane request.
CANDIDATES="$LIMIT"
if [[ "$UNRESOLVED_ONLY" -eq 1 ]]; then
    CANDIDATES=100
    (( CANDIDATES > FETCH )) && CANDIDATES="$FETCH"
fi

rows="$(gh pr list --state "$STATE" --limit "$FETCH" \
        --json number,state,headRefName,title,updatedAt,isDraft,mergedAt 2>/dev/null)" || {
    echo "pr-sessions: could not list PRs (gh not authenticated, or not in a repo)." >&2
    exit 2
}

# A /lastDate window is only as complete as the rows fetched: if the
# fetch came back full, older PRs inside the window may exist beyond it
# and the pool is silently short. Say so rather than presenting a
# truncated window as the window.
if [[ -n "$CUTOFF" ]]; then
    fetched="$(printf '%s' "$rows" | jq 'length' 2>/dev/null || echo 0)"
    if (( fetched >= FETCH )); then
        echo "pr-sessions: fetched the full $FETCH-PR page, so the ${LAST_DATE} window may be" >&2
        echo "  truncated — PRs updated in it can exist further back. Raise it with /lastItem:N." >&2
    fi
fi

# Session = first two path segments of the branch. A branch that does
# not follow the convention (dependabot, a hand-made name) is reported
# as "(unconventional)" rather than silently mis-attributed — a wrong
# owner is worse than a visible unknown.
#
# "Follows the convention" is checked against the WHOLE shape,
# <host>/<clone>/<type>/<desc>, not just "has enough slashes". Counting
# segments alone reported `dependabot/cargo/crates/some-crate/…` as a
# session called "dependabot/nuget", and the footer then told you that
# apparent owner was a parallel session to stay out of the way of. The
# separator is therefore a deny-list of automation vendors, not an
# allow-list of type words — see `conventional` below for why the
# asymmetry is deliberate.
# Rows whose branch does not parse as <host>/<clone>/<type>/<desc>
# cannot be attributed to a session, so any scope filter removes them.
# COUNTED, because removing them silently is exactly how this command
# hid nine PRs and an unanswered P1: the answer looked complete and was
# not.
#
# Counted in the SAME pass that builds `selected`, and from the rows
# left after /lastDate and /lastItem have narrowed the pool. A separate
# pass over the unnarrowed rows reported PRs the caller never asked
# about — `/lastItem:1` would announce an unscopable PR that was outside
# the one-item pool and had therefore not been omitted by anything.
# Sharing the pipeline is what keeps the two answers about the same set.
envelope="$(printf '%s' "$rows" | jq --arg me "$ME" --arg filter "$FILTER" \
        --argjson limit "$CANDIDATES" --arg cutoff "$CUTOFF" \
        --argjson lastitem "${LAST_ITEM:-0}" '
  # STRUCTURAL, not an allow-list of type words.
  #
  # CLAUDE.md §9 specifies `<host>/<clone>/<type>/<short-desc>` and puts
  # no vocabulary on `<type>`. This function used to require one of
  # thirteen conventional-commit words, so every `stage-4/...` and
  # `conformance/...` branch was classified unconventional, given the
  # session "(unconventional)", and then SILENTLY DROPPED by the default
  # clone scope — which is how nine PRs, including one carrying an
  # unanswered P1, became invisible to the command whose whole job is
  # surfacing outstanding work.
  #
  # A branch is conventional if it has the right SHAPE. Anything that
  # parses as a type token counts, because the session is `$p[0]/$p[1]`
  # either way and that is all the scoping needs.
  # Automation vendors, whose branch names also have four or more
  # segments. A DENY-list is right here and an allow-list was wrong
  # above, and the asymmetry is the point: the set of bots that open PRs
  # on a repository is small, known, and changes rarely, while the set of
  # legitimate `<type>` words is open-ended, and every omission silently
  # deletes real work from this listing.
  def bots: ["dependabot","renovate","github-actions","weblate","imgbot",
             "allcontributors","pre-commit-ci","snyk-bot"];
  def conventional:
    (.headRefName | split("/")) as $p
    | ($p | length) >= 4
      and ($p[0] | length) > 0
      and ($p[1] | length) > 0
      and (bots | index($p[0]) == null)
      and ($p[2] | test("^[a-z][a-z0-9._-]*$"));
  def session:
    (.headRefName | split("/")) as $p
    | if conventional then ($p[0] + "/" + $p[1]) else "(unconventional)" end;
  def work:
    (.headRefName | split("/")) as $p
    | if conventional then ($p[2:] | join("/")) else .headRefName end;
  def mark: if (. == $me) then "*" else " " end;
  def pad($n): . + (" " * ($n - length));
  def st:
    if .isDraft then "DRAFT"
    elif .state == "OPEN" then "OPEN"
    elif .state == "MERGED" then "MERGED"
    else "CLOSED" end;

  [ .[] | . + {_s: session, _w: work} ]
  | sort_by(-.number)
  # POOL FIRST: /lastDate then /lastItem, before scope or anything else.
  | ( if $cutoff != "" then map(select(.updatedAt >= $cutoff)) else . end )
  | ( if $lastitem > 0 then .[:$lastitem] else . end )
  # The pool is now fixed, so both answers below describe the same set.
  | . as $pool
  | { # Nothing is dropped by scope when there is no scope, so /all
      # reports zero rather than a number no filter acted on.
      unattributed:
        (if $filter == "" then 0
         else ([$pool[] | select(._s == "(unconventional)")] | length) end),
      rows:
        ($pool
         | ( if $filter == "__MINE__" then map(select(._s == $me))
             elif $filter != "" then map(select(._s | test($filter; "i")))
             else . end )
         | sort_by(-.number) | .[:$limit]) }
')" || {
    # `--session '['` kills jq on the regex, and the unchecked command
    # substitution then left `selected` empty — which the block below
    # renders as "no PRs for a session matching '['", exit 0. A bad
    # pattern and a genuinely empty result must not look alike: one is
    # an invocation error to fix, the other is an answer.
    echo "pr-sessions: could not select rows — check the --session pattern ('$FILTER') is a valid regex." >&2
    exit 2
}

selected="$(printf '%s' "$envelope" | jq '.rows')"
UNATTRIBUTED="$(printf '%s' "$envelope" | jq '.unattributed')"

# Say it on EVERY exit path, not just the one that prints a table.
#
# The warning used to live in the footer alone, so the two paths that
# exit early — no rows at all, and /unresolved finding no known open
# threads — printed a reassuring "no PRs" and omitted the disclosure
# entirely. Those are precisely the answers a reader acts on, and
# precisely the failure this warning exists to prevent: it was itself
# silently dropped exactly where silence was most costly.
unattributed_note() {
    [[ "${UNATTRIBUTED:-0}" -gt 0 ]] || return 0
    echo "  NOTE: $UNATTRIBUTED PR(s) have a branch that does not parse as" >&2
    echo "  <host>/<clone>/<type>/<short-desc>, so they cannot be scoped to a" >&2
    echo "  session and are not listed. Pass /all to see every PR regardless." >&2
}

if [[ "$(printf '%s' "$selected" | jq 'length')" -eq 0 ]]; then
    what="PRs"
    [[ "$FILTER" == "__MINE__" ]] && what="PRs for this clone ($ME)"
    [[ -n "$FILTER" && "$FILTER" != "__MINE__" ]] && what="PRs for a session matching '$FILTER'"
    echo "pr-sessions: no $what in the last $FETCH ${STATE} PR(s)."
    [[ "$DEFAULTED_TO_MINE" -eq 1 ]] && \
        echo "  (scoped to this clone by default — pass /all to see every session)"
    unattributed_note
    exit 0
fi

# ── unresolved review threads, one batched call ─────────────────────
#
# Only for the rows about to be PRINTED, and in a single aliased
# GraphQL query rather than one request per PR: ~0.7s for the whole
# page instead of N round trips.
#
# "Unresolved" is the right count because it is what is still owed —
# not what blocks a merge; nothing does since 2026-08-06. The "!" asks
# a second question the raw count cannot: is the last comment in the
# thread the PR author's? If it is not, somebody is waiting on YOU.
THREAD_JSON='{}'
if [[ "$THREADS" -eq 1 ]]; then
    nums="$(printf '%s' "$selected" | jq -r '.[].number')"
    if [[ -n "$nums" ]]; then
        owner_repo="$(gh repo view --json nameWithOwner --jq .nameWithOwner 2>/dev/null || true)"
        if [[ -n "$owner_repo" ]]; then
            q="{ repository(owner: \"${owner_repo%%/*}\", name: \"${owner_repo##*/}\") {"
            while read -r n; do
                [[ -z "$n" ]] && continue
                q+=" p${n}: pullRequest(number: ${n}) { number author { login }"
                q+=" reviewThreads(first: 100) { pageInfo { hasNextPage } nodes { isResolved"
                q+=" comments(last: 1) { nodes { author { login } } } } } }"
            done <<< "$nums"
            q+=" } }"
            # The PR author is bound BEFORE descending into the threads.
            # Reading `.author.login` inside the thread pipeline compared
            # each last-commenter against a field the thread node does
            # not have — i.e. against null — so every unresolved thread
            # counted as awaiting and every row wore a "!". The bang then
            # said nothing the count had not already said.
            THREAD_JSON="$(gh api graphql -f query="$q" --jq '
                [ .data.repository | to_entries[] | .value
                  | (.author.login // "") as $pr_author
                  | (.reviewThreads.pageInfo.hasNextPage // false) as $truncated
                  | { key: (.number | tostring),
                      value: {
                        # A TRUNCATED PAGE IS UNKNOWN, NOT ZERO. Past 100
                        # threads only the first page arrives, so a PR
                        # whose early threads are all resolved and whose
                        # open one sits later would report "-" and be
                        # dropped from /unresolved — hiding exactly the
                        # outstanding work this command promises to
                        # surface. Same rule as a failed lookup below.
                        unresolved: (if $truncated then null else
                                      ([.reviewThreads.nodes[] | select(.isResolved == false)] | length) end),
                        awaiting:   (if $truncated then null else
                                      ([.reviewThreads.nodes[]
                                        | select(.isResolved == false)
                                        | select((.comments.nodes[0].author.login // "") != $pr_author)] | length) end)
                      } } ] | from_entries' 2>/dev/null)" || THREAD_JSON=""
            # A failed lookup must read as UNKNOWN, never as zero: "0
            # unresolved" is exactly the reassuring answer you would act
            # on, and it would be a guess.
            [[ -z "$THREAD_JSON" ]] && THREAD_JSON="null"
        else
            # No repo context ⇒ no thread data. Same rule: unknown.
            THREAD_JSON="null"
        fi
    fi
fi

# The THR column can honestly print "?" for an unknown count. The
# /unresolved FILTER cannot: it must decide keep-or-drop per row, and
# the only safe default — treat unknown as zero — deletes exactly the
# rows the caller asked to see, then reports "no PRs with unresolved
# review threads" as though the question had been answered. That is the
# unknown-is-not-zero safeguard undone one stage later, so refuse.
if [[ "$UNRESOLVED_ONLY" -eq 1 && "$THREAD_JSON" == "null" ]]; then
    echo "pr-sessions: could not read review threads, so /unresolved cannot be answered." >&2
    echo "  (gh api graphql failed, or the repository could not be resolved)" >&2
    exit 2
fi

out="$(printf '%s' "$selected" | jq -r --arg me "$ME" --argjson grouped "$GROUPED" \
        --argjson th "$THREAD_JSON" --argjson want "$THREADS" \
        --argjson unres "$UNRESOLVED_ONLY" --argjson limit "$LIMIT" '
  def mark: if (. == $me) then "*" else " " end;
  def pad($n): . + (" " * ($n - length));
  def lpad($n): (" " * ($n - length)) + .;
  def st:
    if .isDraft then "DRAFT"
    elif .state == "OPEN" then "OPEN"
    elif .state == "MERGED" then "MERGED"
    else "CLOSED" end;
  def threads:
    if $want == 0 then ""
    elif $th == null then "?"
    else ($th[(.number | tostring)] // null) as $t
      | if $t == null or $t.unresolved == null then "?"
        elif $t.unresolved == 0 then "-"
        else "\($t.unresolved)\(if $t.awaiting > 0 then "!" else "" end)"
        end
    end;

  ( if $unres == 1
      then map(select(
             (($th // {})[(.number | tostring)] // null) as $t
             | if $t == null or $t.unresolved == null then true
               else $t.unresolved > 0 end))
      else . end )
  | sort_by(-.number) | .[:$limit]
  | if $grouped == 1 then
    ( group_by(._s) | sort_by(-(map(.number) | max))
      | map(
          "\n\(.[0]._s)\(if .[0]._s == $me then "   <- this clone" else "" end)"
          , ( sort_by(-.number)[]
              | "  #\(.number)  \(st | pad(6))  \(threads | lpad(3))  \(.updatedAt[0:10])  \(._w)" )
        ) | flatten | .[] )
  else
    ( sort_by(-.number)[]
      | "\(._s | mark) #\(.number | tostring | pad(4))  \(st | pad(6))  \(threads | lpad(3))  \(.updatedAt[0:10])  \(._s | pad(30))  \(._w)" )
  end
')"

if [[ "$GROUPED" -eq 0 ]]; then
    printf '  %-5s  %-6s  %3s  %-10s  %-30s  %s\n' \
        PR STATE THR UPDATED SESSION WORK
fi
if [[ -z "${out//[$' \t\n']/}" ]]; then
    scope="this clone ($ME)"
    [[ "$FILTER" == "" ]] && scope="any session"
    [[ -n "$FILTER" && "$FILTER" != "__MINE__" ]] && scope="sessions matching '$FILTER'"
    echo "pr-sessions: no PRs with unresolved review threads for $scope"
    echo "  (checked the newest $CANDIDATES of the last $FETCH ${STATE} PRs)"
    unattributed_note
    exit 0
fi

printf '%s\n' "$out"

echo
unattributed_note
if [[ "$DEFAULTED_TO_MINE" -eq 1 ]]; then
    echo "  Scoped to this clone ($ME) — pass /all for every session."
elif [[ "$GROUPED" -eq 0 ]]; then
    echo "  * = this clone ($ME).  Others belong to parallel sessions:"
    echo "  do not push to, rebase, delete, or answer reviews on their branches."
fi
