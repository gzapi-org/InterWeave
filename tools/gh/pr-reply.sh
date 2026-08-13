#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/gh/pr-reply.sh
#
# >>> help
# Reply to ONE review thread and resolve it, in a single call.
#
# The body arrives on STDIN and is never interpolated into a command
# line. That is the point of this script, not a detail: replies quote
# code, and a body pasted into a double-quoted `gh api -f body="..."`
# gets `backticks` command-substituted and `$vars` expanded by the shell
# before gh ever sees it. That has already happened here — a reply
# posted with the name of the guard it was describing silently removed,
# because the shell ran it. Reading stdin into a variable and handing
# that variable to `gh -f` closes the whole class.
#
# It also enforces the two rules that are easy to break by hand:
#
#   * ANOTHER SESSION'S PR IS REFUSED. Ownership is the branch prefix,
#     not the PR author (every session pushes as the same GitHub user).
#     The repo-root CLAUDE.md is explicit: never answer reviews on
#     another session's branch. A wrong reply cannot be unsent, and that
#     session is mid-flight on a fix you cannot see.
#   * RESOLVING IS A CLAIM. --no-resolve exists for the case where the
#     reply is a question, or the finding is real and not yet fixed.
#     Resolution stopped gating merges on 2026-08-06, so a resolve now
#     signals only "handled" and nothing downstream catches a false one.
#
# Usage:
#   tools/gh/pr-reply.sh <thread-id> [options]   # body on stdin
#
#   tools/gh/pr-reply.sh PRRT_xxx <<'EOF'
#   Fixed in #123 — the guard now anchors the filter.
#   EOF
#
#   tools/gh/pr-reply.sh PRRT_xxx --no-resolve <<'EOF'
#   Real, but the fix needs the contract change first — leaving open.
#   EOF
#
# Options:
#   --no-resolve   post the reply, leave the thread open
#   --dry-run      show what would be posted, touch nothing
#   -h, --help     this text
#
# Thread ids come from the queue: `/comments`, or the GraphQL in the
# pr-review-backlog skill. They look like PRRT_kwDOS6OPLs6XAV3d.
#
# A quoted heredoc (<<'EOF') is the recommended way to pass the body:
# unquoted, the SHELL expands it before this script is reached, and no
# amount of care in here can undo that.
#
# Exit codes:
#   0  replied (and resolved, unless --no-resolve)
#   2  invocation problem — bad id, empty body, not your PR, gh failure
# <<< help

set -uo pipefail

THREAD=""
RESOLVE=1
DRY_RUN=0

die() { echo "pr-reply: $*" >&2; exit 2; }

while [[ $# -gt 0 ]]; do
    case "$1" in
        --no-resolve) RESOLVE=0; shift ;;
        --dry-run)    DRY_RUN=1; shift ;;
        -h|--help)
            sed -n '/^# >>> help$/,/^# <<< help$/p' "$0" \
                | sed '1d;$d' | sed 's/^# \{0,1\}//'
            exit 0 ;;
        -*) die "unknown option '$1' (try --help)" ;;
        *)
            [[ -z "$THREAD" ]] || die "one thread id at a time (got '$THREAD' and '$1')"
            THREAD="$1"; shift ;;
    esac
done

[[ -n "$THREAD" ]] || die "a review-thread id is required (try --help)"

# Fail on the id shape rather than on a confusing API error. Review
# THREAD ids start PRRT_; a review COMMENT id (PRRC_) is the other thing
# in the same output and cannot be replied to this way.
[[ "$THREAD" =~ ^PRRT_[A-Za-z0-9_-]+$ ]] \
    || die "'$THREAD' is not a review-thread id (expected PRRT_…; PRRC_ is a comment, not a thread)"

for bin in gh jq; do
    command -v "$bin" >/dev/null 2>&1 || die "$bin is required but not installed."
done

# STDIN, never a command-line argument. See the header.
if [[ -t 0 ]]; then
    die "the reply body is read from stdin — pipe it, or use <<'EOF' … EOF"
fi
BODY="$(cat)"
[[ -n "${BODY//[$' \t\n']/}" ]] || die "the reply body is empty."

# ── who owns this thread's PR ───────────────────────────────────────
THREAD_JSON="$(gh api graphql -f query='
    query($id: ID!) {
      node(id: $id) {
        ... on PullRequestReviewThread {
          isResolved path line
          pullRequest { number state headRefName }
          comments(last: 1) { nodes { author { login } } }
        }
      }
    }' -f id="$THREAD" --jq '.data.node' 2>/dev/null)" \
    || die "could not read thread $THREAD (gh not authenticated, or wrong repo)."

[[ -n "$THREAD_JSON" && "$THREAD_JSON" != "null" ]] \
    || die "thread $THREAD not found in this repository."

PR_NUMBER="$(printf '%s' "$THREAD_JSON" | jq -r '.pullRequest.number')"
PR_BRANCH="$(printf '%s' "$THREAD_JSON" | jq -r '.pullRequest.headRefName')"
PR_STATE="$(printf '%s' "$THREAD_JSON" | jq -r '.pullRequest.state')"
IS_RESOLVED="$(printf '%s' "$THREAD_JSON" | jq -r '.isResolved')"
LOCATION="$(printf '%s' "$THREAD_JSON" | jq -r '"\(.path):\(.line // "?")"')"

# The session that owns a branch is its first two segments — the same
# rule pr-sessions.sh marks rows with. Derived here, not passed in, so
# it cannot be talked out of.
ME=""
if root="$(git rev-parse --show-toplevel 2>/dev/null)"; then
    ME="$(hostname -s)/$(basename "$root")"
fi
OWNER="$(printf '%s' "$PR_BRANCH" | cut -d/ -f1,2)"

if [[ -n "$ME" && "$OWNER" != "$ME" ]]; then
    echo "pr-reply: #$PR_NUMBER belongs to '$OWNER', and this clone is '$ME'." >&2
    echo "  Not replying. That session is mid-flight on a fix you cannot see," >&2
    echo "  and a review reply cannot be unsent. Raise it in the PR instead." >&2
    exit 2
fi

if [[ "$IS_RESOLVED" == "true" ]]; then
    echo "pr-reply: thread is already resolved — replying anyway, leaving it resolved." >&2
    RESOLVE=0
fi

if [[ "$DRY_RUN" -eq 1 ]]; then
    echo "would reply to #$PR_NUMBER ($PR_STATE) $LOCATION — thread $THREAD"
    echo "would resolve: $([[ "$RESOLVE" -eq 1 ]] && echo yes || echo no)"
    echo "--- body ---"
    printf '%s\n' "$BODY"
    exit 0
fi

# ── reply ───────────────────────────────────────────────────────────
URL="$(gh api graphql -f query='
    mutation($id: ID!, $body: String!) {
      addPullRequestReviewThreadReply(
        input: {pullRequestReviewThreadId: $id, body: $body}) {
        comment { url }
      }
    }' -f id="$THREAD" -f body="$BODY" \
    --jq '.data.addPullRequestReviewThreadReply.comment.url' 2>/dev/null)" \
    || die "the reply was rejected (thread $THREAD on #$PR_NUMBER)."

[[ -n "$URL" ]] || die "the reply returned no URL — assume it did not post."

echo "replied: $URL"

# ── resolve ─────────────────────────────────────────────────────────
#
# Only after the reply landed. A resolved thread with no reply is the
# worst outcome available: it reads as answered and shows nothing.
if [[ "$RESOLVE" -eq 1 ]]; then
    RESOLVED="$(gh api graphql -f query='
        mutation($id: ID!) {
          resolveReviewThread(input: {threadId: $id}) {
            thread { isResolved }
          }
        }' -f id="$THREAD" \
        --jq '.data.resolveReviewThread.thread.isResolved' 2>/dev/null)" \
        || die "replied, but the resolve failed — thread $THREAD is still open."
    [[ "$RESOLVED" == "true" ]] \
        || die "replied, but the thread did not resolve (got '$RESOLVED')."
    echo "resolved: #$PR_NUMBER $LOCATION"
else
    echo "left open: #$PR_NUMBER $LOCATION"
fi
