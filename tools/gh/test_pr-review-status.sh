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
    printf '{"state":"%s","mergeStateStatus":"CLEAN","headRefOid":"%s","author":{"login":"me"},"isDraft":false,"reviewRequests":%s}\n' \
      "$st" "$head" "$(python3 -c "print('['+','.join(['{}']*$req)+']')")"
    exit 0 ;;
  "repo view")
    echo '{"nameWithOwner":"o/r"}'; exit 0 ;;
  "pr checks")
    echo "some / Check	pass	1s"; exit 0 ;;
  "api graphql")
    echo '[]'; exit 0 ;;
esac

# gh api repos/o/r/pulls/N/reviews
if [[ "$1" == "api" && "$2" == *"/reviews" ]]; then
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
    printf '%s\n' "$2" > "$SANDBOX/state/reviews"
    shift 2
    RUN_OUT="$(PATH="$SANDBOX/bin:$PATH" GH_MOCK_STATE="$SANDBOX/state" \
        timeout 20 bash "$UNDER_TEST" 77 o/r "$@" 2>&1)"
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

echo
if (( failures )); then
    echo "FAILED: $failures assertion(s)" >&2
    exit 1
fi
echo "all assertions passed"
