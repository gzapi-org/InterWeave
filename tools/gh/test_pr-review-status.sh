#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/gh/test_pr-review-status.sh
#
# Behavioural tests for pr-review-status.sh.
#
# The script exists because two GitHub behaviours make the obvious
# reading wrong — a self-reply registers as a REVIEW, and a comment's
# commit_id is re-anchored to the current head. Both produce the SAME
# dangerous answer: "this was reviewed" when it was not. So the tests
# that matter most are the ones asserting a NON-zero exit on input that
# superficially looks reviewed.
#
# The waiting half adds a second dangerous silence: a PR that was
# reviewed and then pushed to will never be reviewed again unless asked,
# and a tool that waits out its timeout there reports "not yet" for a
# state that is permanent. That must exit 5, immediately, without
# sleeping.
#
# The mock serves one scripted poll per `gh pr view` call, so a case can
# say "unreviewed, unreviewed, reviewed" and assert where the loop
# stopped.
#
# Exit codes:
#   0  all assertions passed
#   1  one or more assertions failed

set -uo pipefail

SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
UNDER_TEST="$SCRIPT_DIR/pr-review-status.sh"

[[ -f "$UNDER_TEST" ]] || { echo "test: $UNDER_TEST not found" >&2; exit 1; }

failures=0

# A SELF-TEST CANNOT CATCH AN ASSERTION IT NEVER RAN.
#
# `assert_containss "…" "…"` — a typo, a helper renamed, a helper that
# only ever existed in a sibling suite — is not an error under
# `set -uo pipefail`. bash prints "command not found" to stderr, nothing
# here reads it, and the case asserts NOTHING while the run reports OK.
# This repository shipped two assertions doing exactly that (5f2c0c9),
# and they were found by reading, which is not a mechanism.
#
# bash runs `command_not_found_handle` in a SUBSHELL, so incrementing
# `failures` from inside it is discarded when that subshell exits: the
# handler would print its complaint and the suite would still exit 0 —
# a vacuous guard against vacuous assertions. A FILE survives the
# subshell, so the marker is a file.
#
# The script under test runs as a separate `bash` process, so none of
# this reaches it or masks a genuine missing-command path there.
GUARD_MARKER="$(mktemp)"
command_not_found_handle() {
    printf '%s\n' "$1" >> "$GUARD_MARKER"
    echo "  ✗ self-test bug: called '$1', which this suite does not define" >&2
    return 127
}
SANDBOX=""
cleanup() { [[ -n "$SANDBOX" && -d "$SANDBOX" ]] && rm -rf "$SANDBOX"; }
trap cleanup EXIT

pass() { echo "  ✓ $1"; }
fail() { echo "  ✗ $1" >&2; printf '%s\n' "${2:-}" | sed 's/^/      /' >&2
         failures=$((failures + 1)); }

assert_rc() {
    local label="$1" want="$2"
    if [[ "$RUN_RC" -eq "$want" ]]; then pass "$label"
    else fail "$label — expected exit $want, got $RUN_RC" "$RUN_OUT"; fi
}
assert_contains() {
    if [[ "$RUN_OUT" == *"$2"* ]]; then pass "$1"
    else fail "$1 — output lacked '$2'" "$RUN_OUT"; fi
}
assert_lacks() {
    if [[ "$RUN_OUT" != *"$2"* ]]; then pass "$1"
    else fail "$1 — output unexpectedly contained '$2'" "$RUN_OUT"; fi
}
assert_rc_not() {
    local label="$1" unwanted="$2"
    if [[ "$RUN_RC" -ne "$unwanted" ]]; then pass "$label"
    else fail "$label — expected any exit but $unwanted" "$RUN_OUT"; fi
}

SANDBOX="$(mktemp -d)"
mkdir -p "$SANDBOX/bin" "$SANDBOX/state"

# ── The gh mock ──────────────────────────────────────────────────────
#
# state/polls holds one line per scripted `gh pr view` call:
#   <state>:<headRefOid>:<reviewRequestCount>
# state/reviews holds the reviews array served for EVERY call, as
#   <login>,<commit_id>,<submitted_at> per line.
# The line consumed is tracked in state/n so a poll loop advances.
cat > "$SANDBOX/bin/gh" <<'MOCK'
#!/usr/bin/env bash
S="$GH_MOCK_STATE"

case "$1 ${2:-}" in
  "pr view")
    if [[ "$*" == *"nameWithOwner"* ]]; then
      echo '{"nameWithOwner":"o/r"}'; exit 0
    fi
    n=$(cat "$S/n" 2>/dev/null || echo 0)
    line=$(sed -n "$((n+1))p" "$S/polls")
    [[ -z "$line" ]] && line=$(tail -1 "$S/polls")
    echo $((n+1)) > "$S/n"
    IFS=: read -r st head req <<<"$line"
    [[ "$st" == "FAIL" ]] && exit 1
    # `req` is either a COUNT (the original form, rendered as that many
    # anonymous human requests) or a comma-separated list of LOGINS.
    # --automated-only asks which reviewer a pending request names, and a
    # count cannot answer that.
    if [[ "$req" =~ ^[0-9]+$ ]]; then
      reqs='['; sep=''
      for ((i = 0; i < req; i++)); do reqs="$reqs$sep{\"login\":\"a-human\"}"; sep=','; done
      reqs="$reqs]"
    else
      reqs='['; sep=''
      IFS=, read -ra logins <<<"$req"
      for l in "${logins[@]}"; do reqs="$reqs$sep{\"login\":\"$l\"}"; sep=','; done
      reqs="$reqs]"
    fi
    printf '{"state":"%s","mergeStateStatus":"CLEAN","headRefOid":"%s","author":{"login":"me"},"isDraft":false,"reviewRequests":%s}\n' \
      "$st" "$head" "$reqs"
    exit 0 ;;
  "repo view")
    echo '{"nameWithOwner":"o/r"}'; exit 0 ;;
  "pr checks")
    echo "some / Check	pass	1s"; exit 0 ;;
  "api graphql")
    echo '[]'; exit 0 ;;
esac

# gh api repos/o/r/issues/N/comments — where a CLEAN review lands.
#
# Served from its own state file so a case can have review objects,
# verdict comments, both, or neither. `state/verdicts` holds
#   <login>,<abbreviated-sha>,<created_at>
# per line, and the body is rendered in the real shape so the script's
# own parser is what is being tested rather than a convenient stand-in.
if [[ "$1" == "api" && "$*" == *"/issues/"* && "$*" == *"/comments"* ]]; then
  if [[ -f "$S/verdicts_fail" ]]; then exit 1; fi
  {
    printf '['
    first=1
    while IFS=, read -r login sha at; do
      [[ -z "$login" ]] && continue
      (( first )) || printf ','
      first=0
      printf '{"user":{"login":"%s"},"created_at":"%s","body":"Codex Review: no major issues. **Reviewed commit:** `%s`"}' \
        "$login" "$at" "$sha"
    done < "$S/verdicts" 2>/dev/null
    # `@codex review` asks land in the SAME stream, which is the whole
    # reason the script can see them: they are not review requests.
    # state/asks holds <login>,<created_at> per line.
    while IFS=, read -r login at; do
      [[ -z "$login" ]] && continue
      (( first )) || printf ','
      first=0
      printf '{"user":{"login":"%s"},"created_at":"%s","body":"@codex review please"}' \
        "$login" "$at"
    done < "$S/asks" 2>/dev/null
    printf ']\n'
  }
  exit 0
fi

# gh api repos/o/r/commits/<sha> — when the head was born, which is how
# an ask is told from an answer.
if [[ "$1" == "api" && "$*" == *"/commits/"* ]]; then
  if [[ -f "$S/head_born_fail" ]]; then exit 1; fi
  printf '%s\n' "$(cat "$S/head_born" 2>/dev/null || echo '2026-08-07T09:00:00Z')"
  exit 0
fi

# gh api repos/o/r/pulls/N/reviews
if [[ "$1" == "api" && "$*" == *"/reviews"* ]]; then
  # Matched across ALL args, not positionally: the script passes
  # --paginate, so the URL is no longer $2.
  if [[ -f "$S/reviews_fail" ]]; then exit 1; fi
  {
    printf '['
    first=1
    while IFS=, read -r login commit at; do
      [[ -z "$login" ]] && continue
      (( first )) || printf ','
      first=0
      printf '{"user":{"login":"%s"},"state":"COMMENTED","commit_id":"%s","submitted_at":"%s"}' \
        "$login" "$commit" "$at"
    done < "$S/reviews"
    printf ']\n'
  }
  exit 0
fi
exit 0
MOCK
chmod +x "$SANDBOX/bin/gh"

# NOTE the trailing newline on both writes. `read` returns false on a
# final line that lacks one, so `printf '%s'` here silently dropped the
# last review — three became two, one became none, and every exit-5 case
# degraded into an infinite wait. The bug was in this harness, not the
# script, and it presented as the script being broken.
#
# Every run is bounded. A regression in the "exits immediately" cases
# would otherwise sleep for the full --wait and hang the suite; 20s is
# far above any real path here (the mock never blocks) and far below the
# waits under test.
run() {
    : > "$SANDBOX/state/n"
    printf '%s\n' "$1" > "$SANDBOX/state/polls"
    rm -f "$SANDBOX/state/reviews_fail" "$SANDBOX/state/verdicts_fail"
    printf '%s\n' "$2" > "$SANDBOX/state/reviews"
    # Empty by default: most cases predate verdict comments and must go
    # on meaning what they meant.
    : > "$SANDBOX/state/verdicts"
    # Same for `@codex review` asks. The default head_born sits BEFORE
    # every timestamp these cases use, so an ask written by a case is an
    # ask about the current head unless the case says otherwise.
    : > "$SANDBOX/state/asks"
    rm -f "$SANDBOX/state/head_born_fail"
    printf '2026-08-07T09:00:00Z\n' > "$SANDBOX/state/head_born"
    shift 2
    RUN_OUT="$(PATH="$SANDBOX/bin:$PATH" GH_MOCK_STATE="$SANDBOX/state" \
        timeout 20 bash "$UNDER_TEST" 77 o/r "$@" 2>&1)"
    RUN_RC=$?
    (( RUN_RC == 124 )) && RUN_OUT="TIMED OUT — the run did not return"$'\n'"$RUN_OUT"
}

# Same as `run`, with verdict comments. Separate rather than a fourth
# positional so every existing call keeps its meaning unchanged.
run_v() {
    local verdicts="$3"
    run "$1" "$2" "${@:4}"
    : > "$SANDBOX/state/n"
    printf '%s\n' "$verdicts" > "$SANDBOX/state/verdicts"
    RUN_OUT="$(PATH="$SANDBOX/bin:$PATH" GH_MOCK_STATE="$SANDBOX/state" \
        timeout 20 bash "$UNDER_TEST" 77 o/r "${@:4}" 2>&1)"
    RUN_RC=$?
    (( RUN_RC == 124 )) && RUN_OUT="TIMED OUT — the run did not return"$'\n'"$RUN_OUT"
}

# Same as `run`, plus `@codex review` asks ("<login>,<created_at>" per
# line) and the head's commit date. Separate rather than more positionals
# so every existing call keeps its meaning unchanged.
run_ask() {
    local asks="$3" born="$4"
    run "$1" "$2" "${@:5}"
    : > "$SANDBOX/state/n"
    printf '%s\n' "$asks" > "$SANDBOX/state/asks"
    printf '%s\n' "$born" > "$SANDBOX/state/head_born"
    RUN_OUT="$(PATH="$SANDBOX/bin:$PATH" GH_MOCK_STATE="$SANDBOX/state" \
        timeout 20 bash "$UNDER_TEST" 77 o/r "${@:5}" 2>&1)"
    RUN_RC=$?
    (( RUN_RC == 124 )) && RUN_OUT="TIMED OUT — the run did not return"$'\n'"$RUN_OUT"
}

echo "pr-review-status.sh — the head-reviewed verdict"

run "OPEN:abc123:0" "bot,abc123,2026-08-07T10:00:00Z"
assert_rc "independent review AT the head exits 0" 0
assert_contains "  and says so" "head reviewed?      : yes"

run "OPEN:abc123:0" ""
assert_rc "no reviews at all exits 1" 1

# The PR #363 case: three self-replies are REVIEW objects authored by
# the PR author. They must not read as coverage.
run "OPEN:abc123:0" "me,abc123,2026-08-07T10:00:00Z
me,abc123,2026-08-07T10:01:00Z
me,abc123,2026-08-07T10:02:00Z"
assert_rc "self-authored reviews are NOT coverage" 1
assert_contains "  counted as self" "self reviews        : 3"
assert_contains "  and not as independent" "independent reviews : 0"

# Reviewed at an EARLIER commit, with a review still requested: a review
# is pending, so this is "not yet" (1), not "never coming" (5).
run "OPEN:newhead:1" "bot,oldhead,2026-08-07T10:00:00Z"
assert_rc "stale review WITH a request pending is 'not yet'" 1
assert_contains "  flags the earlier commit" "an EARLIER commit"

echo
echo "pr-review-status.sh — no review is coming (exit 5)"

run "OPEN:newhead:0" "bot,oldhead,2026-08-07T10:00:00Z"
assert_rc "reviewed then pushed, nothing requested, exits 5" 5
assert_contains "  names the cause" "NO REVIEW COMING"

# The distinction that makes 5 worth having: a FRESH PR has had no
# review, but automated review fires on open — so it must NOT be 5.
run "OPEN:abc123:0" ""
assert_rc "a never-reviewed PR is 1, not 5" 1
assert_lacks "  and claims nothing about the cause" "NO REVIEW COMING"

echo
echo "pr-review-status.sh — waiting"

# Poll 1 unreviewed, poll 2 reviewed: the loop must keep going and win.
run "OPEN:abc123:1
OPEN:abc123:1" "" --wait 2 --interval 1 -q
# reviews are static in the mock, so instead assert the loop RAN:
assert_rc "wait expires with nothing and exits 1" 1

run "OPEN:abc123:1" "bot,abc123,2026-08-07T10:00:00Z" --wait 60 --interval 1 -q
assert_rc "wait returns immediately once the head is reviewed" 0

# Exit 5 must not sleep: --wait 3600 with a permanent state has to come
# back at once. If it ever regresses to waiting, this test hangs the
# suite rather than failing quietly — which is the correct alarm.
start=$SECONDS
run "OPEN:newhead:0" "bot,oldhead,2026-08-07T10:00:00Z" --wait 3600 --interval 1 -q
elapsed=$(( SECONDS - start ))
assert_rc "a permanent no-review state exits 5 under a long --wait" 5
if (( elapsed < 5 )); then pass "  and does not sleep first (${elapsed}s)"
else fail "  and does not sleep first" "took ${elapsed}s"; fi

# CLOSED-without-merge: nothing further will arrive, so stop waiting.
run "CLOSED:abc123:1" "" --wait 3600 --interval 1 -q
assert_rc "a CLOSED PR stops the wait and exits 1" 1

echo
echo "pr-review-status.sh — invocation"

RUN_OUT="$(PATH="$SANDBOX/bin:$PATH" GH_MOCK_STATE="$SANDBOX/state" \
    bash "$UNDER_TEST" 2>&1)"; RUN_RC=$?
assert_rc "no PR number exits 2" 2

RUN_OUT="$(PATH="$SANDBOX/bin:$PATH" GH_MOCK_STATE="$SANDBOX/state" \
    bash "$UNDER_TEST" 77 o/r --interval 0 2>&1)"; RUN_RC=$?
assert_rc "zero --interval exits 2" 2

# The SAME duration table wait-merged.sh asserts. as_seconds is
# duplicated across the two standalone scripts on purpose; these paired
# assertions are what stop the copies drifting into accepting different
# things, which is the confusion units were added to remove.
for bad in "" "10sm" "m" "-1" "1x" "10 m"; do
    RUN_OUT="$(PATH="$SANDBOX/bin:$PATH" GH_MOCK_STATE="$SANDBOX/state" \
        bash "$UNDER_TEST" 77 o/r --wait "$bad" 2>&1)"; RUN_RC=$?
    assert_rc "rejects --wait '$bad'" 2
done

# A bare number is seconds; a unit converts. --wait 0 stays the one-shot,
# so a unit form that resolves to a real wait must NOT short-circuit: a
# reviewed head returns 0 immediately either way, which is what makes
# this assertable without sleeping.
run "OPEN:abc123:0" "bot,abc123,2026-08-07T10:00:00Z" --wait 5m --interval 1s -q
assert_rc "accepts --wait 5m --interval 1s" 0
run "OPEN:abc123:0" "bot,abc123,2026-08-07T10:00:00Z" --wait 2h --interval 30s -q
assert_rc "accepts --wait 2h --interval 30s" 0
run "OPEN:abc123:0" "bot,abc123,2026-08-07T10:00:00Z" --wait 300 --interval 1 -q
assert_rc "  and bare seconds still work" 0

RUN_OUT="$(PATH="$SANDBOX/bin:$PATH" GH_MOCK_STATE="$SANDBOX/state" \
    bash "$UNDER_TEST" 77 o/r --nope 2>&1)"; RUN_RC=$?
assert_rc "unknown option exits 2" 2

RUN_OUT="$(PATH="$SANDBOX/bin:$PATH" GH_MOCK_STATE="$SANDBOX/state" \
    bash "$UNDER_TEST" --help 2>&1)"; RUN_RC=$?
assert_rc "--help exits 0" 0
assert_contains "  and documents exit 5" "no review is COMING"

# Three consecutive unreadable polls is unreadable; fewer is a blip.
run "FAIL
FAIL
FAIL" ""
assert_rc "three failed lookups exit 2" 2

echo "pr-review-status: a MERGED PR is never told that no review is coming"
# Review lands on merged PRs here routinely — that is why the post-merge
# sweep exists. Firing exit 5 on one sends the caller to request a review
# instead of awaiting the sweep that was about to deliver it, and
# contradicts the merged-PR exception in the same loop.
# THIS ASSERTION USED TO DO NOTHING. It called `ok`/`bad`, which are
# not functions here -- the helpers are `pass`/`fail`, reached through
# `assert_*`. Bash printed "ok: command not found" to stderr, the
# failure counter never moved, and the suite went on reporting "all
# assertions passed" with this case vacuous. A self-test that cannot
# fail is worse than an absent one: it is the guard reporting coverage
# it does not have.
run "MERGED:newsha:0" "reviewer-a,oldsha1,2026-08-12T10:00:00Z"
assert_rc_not "a merged PR does not exit 5" 5

echo "pr-review-status: a stale review submitted LAST cannot mask head coverage"
# A review created before the last push but submitted after a fresh one
# is newer by timestamp and older by commit. Selecting by recency then
# reports the head unreviewed — and exit 5 claims no review is coming —
# while the review it needed was already there. Coverage is "ANY
# independent review targets head", not "the newest one does".
run "OPEN:abc123:0" "reviewer-a,abc123,2026-08-12T10:00:00Z
reviewer-b,oldsha1,2026-08-12T11:00:00Z"
assert_rc       "head coverage is found despite a newer stale review" 0
assert_contains "  and reports it reviewed"  "head reviewed?      : yes"

echo "pr-review-status: a CLEAN review leaves no review object"
# The reviewer creates a review only when it has something to say. A
# clean pass is an ordinary issue comment naming the commit, so counting
# review objects alone reported `head reviewed? no` on exactly the PRs
# that passed -- and then exit 5, "no review is coming", about a review
# that had already happened and succeeded. CLAUDE.md §9 tells a session
# to wait for the review before arming a security-boundary change, so
# the false negative turned the happy path into an indefinite wait.
run_v "OPEN:abc1234:0" "" "chatgpt-codex-connector,abc1234,2026-08-23T06:23:02Z"
assert_rc "a verdict comment at the head exits 0" 0
assert_contains "  and says the head is reviewed" "head reviewed?      : yes"
assert_contains "  and shows where it came from" "verdict comments    : 1"

echo "pr-review-status: the verdict must name THIS head"
# Inferring coverage from "a comment arrived after the push" would be
# the confident wrong answer this script exists to avoid: a verdict can
# arrive after a push and still describe the commit before it.
run_v "OPEN:9e9e9e9e:0" "" "chatgpt-codex-connector,0d5a1234,2026-08-23T06:23:02Z"
assert_rc "a verdict for an earlier commit is not coverage" 5
assert_contains "  and names the cause" "NO REVIEW COMING"

echo "pr-review-status: an abbreviated sha still matches"
# The body carries ten characters, the PR carries forty.
run_v "OPEN:cf434a7b3318cfe48f5dae7d3eefb2c9767169e8:0" "" "chatgpt-codex-connector,cf434a7b33,2026-08-23T06:23:02Z"
assert_rc "a ten-character verdict sha matches a full head" 0

echo "pr-review-status: a verdict and a stale review object together"
# The real shape of a PR that was reviewed twice with findings and once
# clean. The stale review objects must not mask the clean verdict.
run_v "OPEN:33333333:0" "bot,11111111,2026-08-23T05:30:00Z
bot,22222222,2026-08-23T05:42:00Z" "chatgpt-codex-connector,33333333,2026-08-23T06:23:00Z"
assert_rc "the clean verdict is what covers the head" 0
assert_contains "  reviews are still reported" "independent reviews : 2"

echo "pr-review-status: only a RECOGNISED reviewer can verdict"
# On a public repository anyone may leave an issue comment. Accepting
# any non-author comment containing the verdict phrase let a third party
# mark a head reviewed by typing it -- satisfying the §9 prerequisite
# for auto-merging a security-boundary change without the reviewer ever
# having run. A spoof of the exact gate this feature exists to make
# usable.
run_v "OPEN:abc1234:0" "" "some-passer-by,abc1234,2026-08-23T06:23:02Z"
assert_rc "a stranger's verdict is not coverage" 1
assert_contains "  and is not counted" "verdict comments    : 0"

echo "pr-review-status: the bot is recognised under both spellings"
# The same account appears with and without the `[bot]` suffix
# depending on the API surface. Listing both beats normalising, because
# a normalisation that quietly stopped matching would fail OPEN.
run_v "OPEN:abc1234:0" "" "chatgpt-codex-connector,abc1234,2026-08-23T06:23:02Z"
assert_rc "the bare login is recognised" 0
run_v "OPEN:abc1234:0" "" "chatgpt-codex-connector[bot],abc1234,2026-08-23T06:23:02Z"
assert_rc "the [bot] suffix is recognised too" 0

echo "pr-review-status: the PR author cannot verdict their own PR"
# Same rule as review objects. A session posting the shape of a verdict
# comment would otherwise mark its own head reviewed.
# The author would have to also be a recognised reviewer for this to
# be a real case, so the test forces it: even then, self-review is not
# coverage.
INTERWEAVE_VERDICT_AUTHORS='["me"]' \
run_v "OPEN:abc1234:0" "" "me,abc1234,2026-08-23T06:23:02Z"
assert_rc "a self-authored verdict is not coverage" 1
assert_contains "  and is not counted" "verdict comments    : 0"

echo "pr-review-status: --automated-only closes the review-OBJECT hole"
# THE DEFECT. `independent` is every non-author reviewer, and this
# repository is PUBLIC -- any GitHub user may submit a review object on
# an open PR. One drive-by review carrying the current head therefore
# exits 0, and CLAUDE.md §9 reads that exit as permission to arm --auto
# on a security-boundary change. The gate that exists to hold the merge
# open for the recognised reviewer is satisfied by a stranger.
#
# This is the same hole the verdict-COMMENT path closes above, left open
# on the path that carries more weight.
run "OPEN:abc123:0" "some-passer-by,abc123,2026-08-07T10:00:00Z" --automated-only
assert_rc    "a stranger's review is not coverage under the flag" 1
assert_contains "  head is reported unreviewed" "head reviewed?      : no"
assert_contains "  and the exclusion is disclosed, not silent" \
                "[1 not the recognised reviewer — NOT counted]"

run "OPEN:abc123:0" "chatgpt-codex-connector,abc123,2026-08-07T10:00:00Z" --automated-only
assert_rc "the recognised reviewer IS coverage under the flag" 0

echo "pr-review-status: the default mode discloses unrecognised coverage"
# The flag is off by default because "was this reviewed by anyone" is a
# real question. But a caller who FORGOT the flag reads an exit code, so
# the exposure has to be visible without it. Coverage resting entirely
# on an unrecognised account says so.
run "OPEN:abc123:0" "some-passer-by,abc123,2026-08-07T10:00:00Z"
assert_rc       "still exits 0 — a human review is real coverage" 0
assert_contains "  but names what is carrying it" "carried ONLY by an unrecognised account"
assert_contains "  and points at the flag"        "--automated-only"

run "OPEN:abc123:0" "chatgpt-codex-connector,abc123,2026-08-07T10:00:00Z"
assert_rc    "recognised coverage exits 0" 0
assert_lacks "  and says nothing about unrecognised accounts" \
             "carried ONLY by an unrecognised account"

# A CLEAN review leaves a verdict comment and no review object, and the
# allow-list already gates those -- so it must not trip the disclosure.
run_v "OPEN:abc1234:0" "" "chatgpt-codex-connector,abc1234,2026-08-23T06:23:02Z"
assert_rc    "a clean verdict covers the head" 0
assert_lacks "  and is not called unrecognised" \
             "carried ONLY by an unrecognised account"

echo "pr-review-status: a stranger cannot make the reviewer look done"
# `no_review_coming` needs a PRIOR review to conclude "this PR is one the
# reviewer has already answered". Counting a stranger there reports
# NO REVIEW COMING (exit 5) about a first automated review still on its
# way -- and §9 then sends the session off to re-request one it already
# has in flight.
run "OPEN:newhead:0" "some-passer-by,oldhead,2026-08-07T10:00:00Z" --automated-only
assert_rc "a stranger's stale review does not trigger exit 5" 1

run "OPEN:newhead:0" "chatgpt-codex-connector,oldhead,2026-08-07T10:00:00Z" --automated-only
assert_rc "the recognised reviewer's stale review does" 5

echo "pr-review-status: --automated-only narrows the pending REQUEST too"
# `requested` suppresses exit 5. A mode that narrows coverage and leaves
# this term reading everybody keeps waiting on a HUMAN reviewer while
# reporting about a bot review that is not coming -- the two terms would
# be answering questions about different populations.
run "OPEN:newhead:a-human" "chatgpt-codex-connector,oldhead,2026-08-07T10:00:00Z" --automated-only
assert_rc "a pending HUMAN request does not suppress the verdict" 5
assert_contains "  and the request is reported as absent for this question" \
                "review requested?   : no"

run "OPEN:newhead:chatgpt-codex-connector" "chatgpt-codex-connector,oldhead,2026-08-07T10:00:00Z" --automated-only
assert_rc "a pending RECOGNISED request does suppress it" 1

run "OPEN:newhead:a-human" "chatgpt-codex-connector,oldhead,2026-08-07T10:00:00Z"
assert_rc "and without the flag a human request still suppresses it" 1

echo "pr-review-status: an @codex review in flight is not 'nothing is coming'"
# THE DEFECT §9 DOCUMENTED INSTEAD OF FIXING. `@codex review` is how a
# review is asked for here and it is NOT a GitHub review request, so
# `reviewRequests` stays empty and the stale-review branch fired
# NO REVIEW COMING seconds after the ask. §9 tells a session to request a
# review and then wait — so the tool answered "waiting cannot help"
# about the exact sequence §9 prescribes.
#
# Head born 09:00, review of an OLD commit at 10:00, ask at 11:00.
run_ask "OPEN:newhead:0" "chatgpt-codex-connector,oldhead,2026-08-07T10:00:00Z" \
        "me,2026-08-07T11:00:00Z" "2026-08-07T09:00:00Z"
assert_rc       "an ask after this head was pushed suppresses exit 5" 1
assert_contains "  and the report names it"      "@codex review asked"
assert_contains "  who asked, not just when"     "by me"
assert_contains "  and that it is outstanding"   "in flight"

# An ask made BEFORE this head existed asked about the PREVIOUS one.
# Without the head's own commit date this could only be compared against
# the newest review, and a review of an earlier commit landing after a
# newer push reads as the answer to an ask it never saw.
run_ask "OPEN:newhead:0" "chatgpt-codex-connector,oldhead,2026-08-07T10:00:00Z" \
        "me,2026-08-07T08:00:00Z" "2026-08-07T09:00:00Z"
assert_rc "an ask older than the head does not suppress it" 5

# The other direction, which requiring a head-matching review would
# break: asked, reviewed, then pushed again WITHOUT asking. No review can
# ever match the new head, so a naive "is there a review for this head"
# test would keep the ask pending forever and exit 5 could never fire.
run_ask "OPEN:thirdhead:0" "chatgpt-codex-connector,secondhead,2026-08-07T11:30:00Z" \
        "me,2026-08-07T11:00:00Z" "2026-08-07T12:00:00Z"
assert_rc "a push after the ask lets exit 5 fire again" 5

echo "pr-review-status: an unreadable head date waits rather than concluding"
# The fallback direction matters: unreadable must mean PENDING. Guessing
# "answered" turns a lookup failure into a confident "nothing is coming"
# about a review that is on its way.
run_ask "OPEN:newhead:0" "chatgpt-codex-connector,oldhead,2026-08-07T10:00:00Z" \
        "me,2026-08-07T08:00:00Z" "2026-08-07T09:00:00Z"
: > "$SANDBOX/state/n"
touch "$SANDBOX/state/head_born_fail"
RUN_OUT="$(PATH="$SANDBOX/bin:$PATH" GH_MOCK_STATE="$SANDBOX/state" \
    timeout 20 bash "$UNDER_TEST" 77 o/r 2>&1)"; RUN_RC=$?
rm -f "$SANDBOX/state/head_born_fail"
assert_rc "an unreadable head date keeps the ask pending" 1

echo "pr-review-status: a covered head does not report an unanswered ask"
# Decides no exit — head_reviewed wins first — but a line reading
# "nothing has answered it yet" beside "head reviewed? yes" is false.
run_ask "OPEN:abc123:0" "chatgpt-codex-connector,abc123,2026-08-07T10:00:00Z" \
        "me,2026-08-07T11:00:00Z" "2026-08-07T09:00:00Z"
assert_rc       "the head is covered" 0
assert_contains "  and the ask reads as answered" "already answered"
assert_lacks    "  not as outstanding"            "in flight"

echo "pr-review-status: --help documents the flag §9 now requires"
RUN_OUT="$(PATH="$SANDBOX/bin:$PATH" bash "$UNDER_TEST" --help 2>&1)"; RUN_RC=$?
assert_rc       "exits 0" 0
assert_contains "names the flag"        "--automated-only"
assert_contains "says why it exists"    "repository is PUBLIC"
assert_contains "and that the bare command is not enough" \
                "does not satisfy"

echo "pr-review-status: a failed comments lookup is UNREADABLE, not empty"
# Same contract as the reviews endpoint: swallowing the failure would
# report a clean review as no review, which is the defect this whole
# section exists to close.
: > "$SANDBOX/state/n"
printf '%s\n' "OPEN:abc123:0" > "$SANDBOX/state/polls"
: > "$SANDBOX/state/reviews"
: > "$SANDBOX/state/verdicts"
touch "$SANDBOX/state/verdicts_fail"
RUN_OUT="$(PATH="$SANDBOX/bin:$PATH" GH_MOCK_STATE="$SANDBOX/state" \
    timeout 20 bash "$UNDER_TEST" 77 o/r 2>&1)"
RUN_RC=$?
rm -f "$SANDBOX/state/verdicts_fail"
assert_rc "a failing comments endpoint exits 2, not 1" 2

echo "pr-review-status: a failed reviews lookup is UNREADABLE, not empty"
# The script's whole job is answering "was this really reviewed". A rate
# limit, permission gap, or transient 5xx converted into [] would produce
# the confident wrong answer "no reviews" and let a caller conclude that
# a reviewed PR was never looked at. It must feed the consecutive-failure
# counter instead, which is what the exit-2 contract already promises.
run "OPEN:abc123:0
OPEN:abc123:0
OPEN:abc123:0" ""
: > "$SANDBOX/state/reviews_fail"
RUN_OUT="$(PATH="$SANDBOX/bin:$PATH" GH_MOCK_STATE="$SANDBOX/state" \
    timeout 20 bash "$UNDER_TEST" 77 o/r 2>&1)"; RUN_RC=$?
assert_rc    "a failing reviews endpoint exits 2, not 1"  2
assert_lacks "  and never claims zero coverage"           "independent reviews : 0"
rm -f "$SANDBOX/state/reviews_fail"


# Consulted BEFORE the pass/fail summary: a suite whose assertions never
# ran has not passed, whatever its counter says.
if [[ -s "$GUARD_MARKER" ]]; then
    echo "test_pr-review-status: FAILED — called $(sort -u "$GUARD_MARKER" | wc -l | tr -d " ") command(s) this suite does not define:" >&2
    sort -u "$GUARD_MARKER" | sed 's/^/      /' >&2
    echo "      Those assertions did not run. Exit 0 would have been a lie." >&2
    rm -f "$GUARD_MARKER"
    exit 1
fi
rm -f "$GUARD_MARKER"
echo
if (( failures )); then
    echo "FAILED: $failures assertion(s)" >&2
    exit 1
fi
echo "all assertions passed"
