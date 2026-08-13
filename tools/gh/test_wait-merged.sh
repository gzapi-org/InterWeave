#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/gh/test_wait-merged.sh
#
# Behavioural tests for wait-merged.sh.
#
# The script's whole value is its EXIT — a caller is asleep until then,
# so every terminal state has to produce one, and produce the right one.
# The dangerous failure is silence that looks like patience: a closed
# PR, a deleted PR, or a revoked token must not be indistinguishable
# from "still merging".
#
# The mock serves one scripted poll per call as the JSON `gh pr view`
# returns, so a case can say "OPEN, OPEN, MERGED" — or
# "OPEN:BLOCKED:some / Check" — and assert where the loop stopped.
#
# Exit codes:
#   0  all assertions passed
#   1  one or more assertions failed

set -uo pipefail

SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
UNDER_TEST="$SCRIPT_DIR/wait-merged.sh"

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

cat > "$SANDBOX/bin/gh" <<'MOCK'
#!/usr/bin/env bash
# One scripted line per call, rendered as the JSON `gh pr view --json`
# returns. A line is "<state>[:<mergeStateStatus>[:<failing check>]]";
# "FAIL" makes that call fail the way a dropped connection does. The
# `gh pr checks` subcommand is answered from the same line: the third
# field, when present, is a failing REQUIRED check.
set -uo pipefail
# The arming probe (gh api graphql) and `gh repo view` are answered
# from fixtures; neither counts as a poll.
if [[ "${1:-}" == "api" ]]; then
  # The stall diagnoses reach for two more endpoints. Both answer from
  # fixtures and default to "nothing wrong", so every pre-existing case
  # keeps its old behaviour.
  case "${2:-}" in
    # Repo-WIDE runs (no head_sha filter): has CI ever fired here at
    # all? Defaults to 1 so every pre-existing case describes a live
    # repository. Must be tested before the head-scoped branch below.
    *actions/runs*)          cat "$GH_MOCK_STATE/runs_count" 2>/dev/null || echo 1; exit 0 ;;
    # Effective branch rules. The missing-run diagnosis asks whether any
    # check is REQUIRED on the base branch, because that is what decides
    # whether an absent run blocks anything. Defaults to one required
    # context so every pre-existing case still describes a protected
    # branch that is waiting on CI.
    # Answered pre-reduced, as every other endpoint here is: the script
    # passes `-q` and real gh applies the filter, so the mock returns
    # what that filter would have produced — the count of required
    # contexts on the base branch.
    *rules/branches*)   cat "$GH_MOCK_STATE/required_checks" 2>/dev/null || echo 1; exit 0 ;;
    # Serves the real JSON shape now, not a pre-reduced number: the
    # script reads netAmount AND minute quantity from one response, so a
    # mock that answers with a bare figure could not exercise the
    # usage-against-limit branch at all.
    *billing/usage*)
      printf '{"usageItems":[{"product":"actions","unitType":"Minutes","quantity":%s,"netAmount":%s}]}\n' \
        "$(cat "$GH_MOCK_STATE/billing_mins" 2>/dev/null || echo 0)" \
        "$(cat "$GH_MOCK_STATE/billing_net"  2>/dev/null || echo 0)"
      exit 0 ;;
  esac
  cat "$GH_MOCK_STATE/arming" 2>/dev/null || echo "queued"
  exit 0
fi
if [[ "${1:-}" == "repo" ]]; then
  echo "testrepo"
  exit 0
fi
# The missing-run probe asks for the head SHA.
if [[ "${2:-}" == "view" && "$*" == *headRefOid* ]]; then
  echo "deadbeefcafe"
  exit 0
fi
# ...and for the base branch, to look up its required checks.
if [[ "${2:-}" == "view" && "$*" == *baseRefName* ]]; then
  echo "main"
  exit 0
fi
if [[ "${2:-}" == "checks" ]]; then
  n=$(cat "$GH_MOCK_STATE/calls" 2>/dev/null || echo 1)
else
  n=$(( $(cat "$GH_MOCK_STATE/calls" 2>/dev/null || echo 0) + 1 ))
  echo "$n" > "$GH_MOCK_STATE/calls"
fi
line="$(sed -n "${n}p" "$GH_MOCK_STATE/states" 2>/dev/null)"
[[ -z "$line" ]] && line="$(tail -n1 "$GH_MOCK_STATE/states")"
[[ "$line" == "FAIL" ]] && exit 1
state="${line%%:*}"; rest="${line#"$state"}"; rest="${rest#:}"
merge="${rest%%:*}"; check="${rest#"$merge"}"; check="${check#:}"

# `gh pr checks` answers the REQUIRED set only — the third field names a
# failing required check. gh exits non-zero when anything is failing,
# which the script must tolerate.
if [[ "${2:-}" == "checks" ]]; then
  if [[ -n "$check" ]]; then
    # "!name" marks a CANCELLED required check rather than a failed one.
    if [[ "$check" == !* ]]; then
      printf '[{"name":"%s","bucket":"cancel"}]\n' "${check#!}"
    else
      printf '[{"name":"%s","bucket":"fail"}]\n' "$check"
    fi
    exit 1
  fi
  # No required checks at all — what a head SHA with no workflow run
  # looks like.
  if [[ -f "$GH_MOCK_STATE/checks_empty" ]]; then
    printf '[]\n'
    exit 0
  fi
  printf '[{"name":"ban-checks / ADR ban-checks","bucket":"pass"}]\n'
  exit 0
fi

printf '{"state":"%s","mergeStateStatus":"%s"}\n' "$state" "${merge:-CLEAN}"
MOCK
chmod +x "$SANDBOX/bin/gh"

# githubstatus.com, answered from a fixture. Defaults to operational so
# no pre-existing case sees an outage.
cat > "$SANDBOX/bin/curl" <<'CURLMOCK'
#!/usr/bin/env bash
set -uo pipefail
status="$(cat "$GH_MOCK_STATE/actions_status" 2>/dev/null || echo operational)"
printf '{"components":[{"name":"Actions","status":"%s"}]}\n' "$status"
CURLMOCK
chmod +x "$SANDBOX/bin/curl"

# jq parses the mock's JSON in the script under test, and the
# dependency check requires it on PATH.
command -v jq >/dev/null 2>&1 || { echo "test: jq required" >&2; exit 1; }

states() { printf '%s\n' "$@" > "$SANDBOX/state/states"; : > "$SANDBOX/state/calls";
           printf 'queued\n' > "$SANDBOX/state/arming"
           rm -f "$SANDBOX/state/checks_empty"
           printf 'operational\n' > "$SANDBOX/state/actions_status"
           printf '1\n' > "$SANDBOX/state/runs_count"
           printf '1\n' > "$SANDBOX/state/required_checks"
           printf '0\n' > "$SANDBOX/state/billing_net"
           printf '0\n' > "$SANDBOX/state/billing_mins"
           unset INTERWEAVE_ACTIONS_INCLUDED_MINUTES; }
no_checks()       { : > "$SANDBOX/state/checks_empty"; }
actions_status()  { printf '%s\n' "$1" > "$SANDBOX/state/actions_status"; }
runs_count()      { printf '%s\n' "$1" > "$SANDBOX/state/runs_count"; }
required_checks() { printf '%s\n' "$1" > "$SANDBOX/state/required_checks"; }
billing_net()     { printf '%s\n' "$1" > "$SANDBOX/state/billing_net"; }
billing_mins()    { printf '%s\n' "$1" > "$SANDBOX/state/billing_mins"; }
arming() { printf '%s\n' "$1" > "$SANDBOX/state/arming"; }
calls()  { cat "$SANDBOX/state/calls" 2>/dev/null || echo 0; }

invoke() {
    RUN_OUT="$(env PATH="$SANDBOX/bin:$PATH" GH_MOCK_STATE="$SANDBOX/state" \
        bash "$UNDER_TEST" "$@" 2>&1)"
    RUN_RC=$?
}

# STDOUT only — a caller that redirects stdout to capture the result
# must receive every verdict, not just the happy ones.
invoke_stdout() {
    RUN_OUT="$(env PATH="$SANDBOX/bin:$PATH" GH_MOCK_STATE="$SANDBOX/state" \
        bash "$UNDER_TEST" "$@" 2>/dev/null)"
    RUN_RC=$?
}

echo "wait-merged: a merge ends the watch"
states MERGED
invoke 429 --interval 1 --timeout 10
assert_rc       "exits 0" 0
assert_contains "says it merged"        "MERGED"
assert_contains "says what is now safe" "return to main"

echo "wait-merged: it keeps waiting while the PR is open"
states OPEN OPEN MERGED
invoke 429 --interval 1 --timeout 30
assert_rc "exits 0 once merged" 0
if [[ "$(calls)" -eq 3 ]]; then
    pass "polled until the state changed (3 calls)"
else
    fail "wrong number of polls" "calls=$(calls)"
fi

echo "wait-merged: a CLOSED PR is not a success"
# The case that looks exactly like patience if you only watch for
# MERGED — and the one where returning to main would be wrong.
states OPEN CLOSED
invoke 429 --interval 1 --timeout 30
assert_rc       "exits 3, not 0" 3
assert_contains "says it was not merged" "WITHOUT MERGING"
assert_contains "warns against moving"   "do not return to main"

echo "wait-merged: the timeout ends the watch rather than hanging"
states OPEN
invoke 429 --interval 1 --timeout 3
assert_rc       "exits 4" 4
assert_contains "says the watch expired" "expired"

echo "wait-merged: one failed lookup does not kill the watch"
# A dropped connection or a rate-limit pause must not end a watch whose
# caller is asleep — they would learn nothing at all.
states OPEN FAIL OPEN MERGED
invoke 429 --interval 1 --timeout 30
assert_rc "rode out the blip and exited 0" 0

echo "wait-merged: repeated failure is reported, not waited out"
# A deleted PR or a revoked token. Silence until timeout would be the
# worst answer available.
states FAIL FAIL FAIL FAIL FAIL
invoke 429 --interval 1 --timeout 60
assert_rc       "exits 2" 2
assert_contains "says it gave up reading" "UNREADABLE"

echo "wait-merged: a failed check ends the watch instead of timing out"
# OPEN + BLOCKED with a check already concluded badly will never merge.
# Waiting it out would spend the whole timeout to report nothing, when
# the actionable fact was there on the first poll.
states "OPEN:BLOCKED:backend / Build"
invoke 429 --interval 1 --timeout 30
assert_rc       "exits 5" 5
assert_contains "says it is blocked"     "BLOCKED"
assert_contains "names the failed check" "backend / Build"

echo "wait-merged: an OPTIONAL failing check does not end the watch"
# Only required checks stop the queue. Reading the raw rollup counted
# every check, so one failing optional job reported a healthy PR as
# dead. The mock's `pr checks --required` answer is what the script now
# reads, and it lists no failure here.
states "OPEN:BLOCKED" "MERGED:CLEAN"
invoke 429 --interval 1 --timeout 30
assert_rc "waited past the optional failure and exited 0" 0

echo "wait-merged: the required-check lookup tolerates gh's non-zero exit"
# gh exits non-zero when checks are failing — a status, not an error.
states "OPEN:BLOCKED:required / Thing"
invoke 429 --interval 1 --timeout 30
assert_rc       "still exits 5, not 2" 5
assert_contains "names the check" "required / Thing"

echo "wait-merged: a timeout shorter than the interval is still honoured"
# `--interval 60 --timeout 30` used to expire on the FIRST poll while
# claiming it had waited 30 seconds.
states OPEN
start=$SECONDS
invoke 429 --interval 60 --timeout 2
waited=$(( SECONDS - start ))
assert_rc "exits 4" 4
if (( waited >= 2 )); then
    pass "actually waited the advertised timeout (${waited}s)"
else
    fail "expired early" "waited ${waited}s, timeout was 2s"
fi

echo "wait-merged: a CANCELLED required check is terminal too"
# It cannot let the PR merge without a re-run, so waiting is pointless.
# This is not hypothetical: on 2026-08-06 an Actions outage evicted PRs
# from the queue, GitHub cancelled 13 jobs in the abandoned merge-group
# run, and three watches expired at 40 minutes apiece reporting "state
# unknown" because only `fail` counted.
states "OPEN:BLOCKED:!contracts / Contracts — JSON Schema validation"
invoke 429 --interval 1 --timeout 30
assert_rc       "exits 5, not 4" 5
assert_contains "names the cancelled check" "contracts / Contracts"

echo "wait-merged: a conflicted branch ends the watch"
states "OPEN:DIRTY"
invoke 429 --interval 1 --timeout 30
assert_rc       "exits 5" 5
assert_contains "says what to do" "fold origin/main in"

echo "wait-merged: BLOCKED with checks merely pending keeps waiting"
# The normal path through the queue. Treating this as terminal would
# make the watcher useless for every healthy PR.
states "OPEN:BLOCKED" "OPEN:BLOCKED" "OPEN:CLEAN" "MERGED:CLEAN"
invoke 429 --interval 1 --timeout 30
assert_rc "waited through BLOCKED and exited 0" 0
if [[ "$(calls)" -eq 4 ]]; then
    pass "did not stop early (4 polls)"
else
    fail "stopped at the wrong poll" "calls=$(calls)"
fi

echo "wait-merged: a merge still wins over a stale failing check"
# Order matters: MERGED is checked before the blocked heuristics, so a
# PR that landed is reported as landed whatever the rollup says.
states "MERGED:CLEAN:some / OldCheck"
invoke 429 --interval 1 --timeout 10
assert_rc       "exits 0" 0
assert_contains "reports the merge" "MERGED"

echo "wait-merged: --quiet suppresses progress but never the verdict"
states MERGED
invoke 429 --interval 1 --timeout 10 --quiet
assert_rc       "exits 0" 0
assert_contains "still reports the merge" "MERGED"
if [[ "$RUN_OUT" != *"watching #"* ]]; then
    pass "no progress chatter"
else
    fail "progress line survived --quiet" "$RUN_OUT"
fi

echo "wait-merged: every verdict is one stdout line naming its exit code"
# The code is what a caller branches on; the line is what a person
# reads. "exit 5" alone does not say which of the two blocked cases it
# was, and a verdict on stderr is lost by anyone capturing stdout.
states MERGED
invoke_stdout 429 --interval 1 --timeout 10
assert_rc       "merged exits 0" 0
assert_contains "on stdout, with the code" "MERGED — safe to return to main (exit 0)"

states CLOSED
invoke_stdout 429 --interval 1 --timeout 10
assert_rc       "closed exits 3" 3
assert_contains "on stdout, with the code" "(exit 3)"

states "OPEN:BLOCKED:required / Thing"
invoke_stdout 429 --interval 1 --timeout 10
assert_rc       "blocked exits 5" 5
assert_contains "names which blocked case" "these required checks failed: required / Thing"
assert_contains "and the code"             "(exit 5)"

states "OPEN:DIRTY"
invoke_stdout 429 --interval 1 --timeout 10
assert_rc       "conflict exits 5" 5
assert_contains "names the other blocked case" "conflicts with the base"

states OPEN
invoke_stdout 429 --interval 1 --timeout 2
assert_rc       "expiry exits 4" 4
assert_contains "expiry reaches stdout too" "STILL OPEN"
assert_contains "with its code"             "(exit 4)"

states FAIL FAIL FAIL FAIL FAIL
invoke_stdout 429 --interval 1 --timeout 60
assert_rc       "unreadable exits 2" 2
assert_contains "unreadable reaches stdout too" "UNREADABLE"
assert_contains "with its code"                 "(exit 2)"

echo "wait-merged: a usage error is not a verdict"
# Invocation errors stay on stderr — they are not results of the job.
invoke_stdout abc
assert_rc "exits 2" 2
if [[ -z "${RUN_OUT//[$' \t\n']/}" ]]; then
    pass "nothing on stdout"
else
    fail "usage error leaked into the result stream" "$RUN_OUT"
fi

echo "wait-merged: it says up front when nothing will merge the PR"
# A PR neither queued nor armed sits green forever. Spending the whole
# budget to report "still open" is true and useless — this is the other
# half of the 2026-08-06 incident, where repeated queue evictions left
# PRs looking armed but unqueued and three watches expired before
# anyone noticed.
states OPEN OPEN MERGED
arming idle
invoke 429 --interval 1 --timeout 30
assert_rc       "still watches (arming may follow)" 0
assert_contains "warns immediately"   "neither queued nor auto-merge-armed"
assert_contains "says how to fix it"  "gh pr merge 429 --merge --auto"

echo "wait-merged: an armed or queued PR draws no warning"
states MERGED
arming queued
invoke 429 --interval 1 --timeout 10
assert_rc "exits 0" 0
if [[ "$RUN_OUT" != *"neither queued nor"* ]]; then
    pass "quiet when the PR is on its way"
else
    fail "warned about a queued PR" "$RUN_OUT"
fi

echo "wait-merged: invocation errors"
states MERGED
invoke
assert_rc       "a missing PR number is refused" 2
assert_contains "says what is required" "PR number is required"

invoke abc
assert_rc "a non-numeric PR is refused" 2

invoke 0
assert_rc "zero is refused" 2

invoke 429 431
assert_rc "two PR numbers are refused" 2

invoke 429 --interval
assert_rc "a missing --interval value is refused" 2

invoke 429 --interval x
assert_rc       "a non-numeric --interval is refused" 2
assert_contains "names the expectation" "duration like 90, 90s, 10m or 2h"

invoke 429 --timeout 0
assert_rc "a zero --timeout is refused" 2

invoke 429 --nope
assert_rc       "an unknown option is refused" 2
assert_contains "points at --help" "--help"

echo "wait-merged: --help lists the flags and the exit codes"
invoke --help
assert_rc       "exits 0" 0
assert_contains "documents --interval" "--interval"
assert_contains "documents --timeout"  "--timeout"
assert_contains "documents --quiet"    "--quiet"
assert_contains "documents exit 3"     "3  closed"
assert_contains "documents exit 5"     "5  blocked"

# ── The stall diagnoses ─────────────────────────────────────────────
#
# `STILL OPEN — state unknown` is the answer that taught nobody
# anything. Each of these asserts that a knowable cause is NAMED instead,
# and — just as important — that a healthy watch never sees them.

echo "wait-merged: an Actions outage is named, not waited out"
states "OPEN:BLOCKED"
actions_status "major_outage"
no_checks
invoke 431 --interval 1 --timeout 6
assert_rc       "exits 6, not 4"            6
assert_contains "names the platform"        "GitHub Actions is major_outage"
assert_contains "absolves the PR"           "the PR is fine"

echo "wait-merged: a head commit with no workflow run is named"
states "OPEN:BLOCKED"
no_checks
runs_count 0
invoke 431 --interval 1 --timeout 6
assert_rc       "exits 6"                   6
assert_contains "names the lost webhook"    "no workflow run exists"
assert_contains "says to re-arm afterwards" "RE-ARM"

echo "wait-merged: a spent Actions allowance is named"
states "OPEN:BLOCKED"
no_checks
billing_net "4.20"
invoke 431 --interval 1 --timeout 6
assert_rc       "billed overage exits 6"    6
assert_contains "names the allowance"       "allowance is spent"

# The case netAmount alone CANNOT see, and the reason this function was
# rewritten on 2026-08-07. A plan that does not purchase overage is never
# billed: net stays 0 while GitHub quietly stops handing out runners. The
# old check read that as "allowance intact" and expired at plain 4,
# leaving the one state exit 6 exists to name permanently unreachable.
states "OPEN:BLOCKED"
no_checks
billing_net "0"
billing_mins "50000"
export INTERWEAVE_ACTIONS_INCLUDED_MINUTES=50000
invoke 431 --interval 1 --timeout 6
assert_rc       "usage at the configured limit exits 6, with net 0"  6
assert_contains "  and names the allowance"                          "allowance is spent"

# Same plan, minutes to spare: must NOT fire, or every ordinary slow PR
# on a healthy account becomes a false alarm.
states "OPEN:BLOCKED"
no_checks
billing_net "0"
billing_mins "3072"
export INTERWEAVE_ACTIONS_INCLUDED_MINUTES=50000
invoke 431 --interval 1 --timeout 6
assert_rc       "usage well under the limit stays exit 4"  4
assert_lacks    "  and says nothing about the allowance"   "allowance is spent"

# No allowance configured: usage alone cannot say what is left, so the
# honest answer is to decline rather than guess in either direction.
states "OPEN:BLOCKED"
no_checks
billing_net "0"
billing_mins "999999"
invoke 431 --interval 1 --timeout 6
assert_rc       "huge usage with NO configured allowance stays exit 4"  4
assert_lacks    "  and does not invent a verdict"                       "allowance is spent"

echo "wait-merged: durations accept units, and mean the same thing"
# The startup line prints the CONVERTED seconds, so this asserts the
# arithmetic and not merely that the flag parsed. pr-review-status.sh
# carries an identical copy of as_seconds and its suite asserts the same
# table — that pairing is what keeps the two from drifting apart.
#
# MERGED on the FIRST poll, deliberately: the script honours the timeouts
# it is given, so asserting `--timeout 10m` against an OPEN PR would sit
# here for ten real minutes. A terminal first state prints the converted
# figures and exits at once.
states "MERGED"
invoke 431 --timeout 10m --interval 2m
assert_contains "10m/2m convert to seconds" "every 120s, giving up after 600s"
states "MERGED"
invoke 431 --timeout 600 --interval 120
assert_contains "  bare numbers still mean seconds" "every 120s, giving up after 600s"
states "MERGED"
invoke 431 --timeout 1h --interval 30s
assert_contains "  1h and 30s convert too" "every 30s, giving up after 3600s"

# Rejections exit before any polling, so these cost nothing.
for bad in "" "10sm" "m" "-5" "1x" "10 m"; do
    states "MERGED"
    invoke 431 --timeout "$bad"
    assert_rc "rejects --timeout '$bad'" 2
done
states "MERGED"
invoke 431 --timeout 0
assert_rc       "rejects --timeout 0"        2
assert_contains "  as greater-than-zero"     "greater than zero"

echo "wait-merged: a healthy watch never diagnoses"
# Everything nominal: checks reporting, Actions up, runs present, net 0.
# The expiry must stay the plain exit 4 — a diagnosis firing here would
# turn every ordinary slow PR into a false alarm.
states "OPEN:BLOCKED"
invoke 431 --interval 1 --timeout 3
assert_rc       "still exits 4"             4
assert_contains "still the plain answer"    "watch expired, state unknown"

echo "wait-merged: pending checks are not a missing run"
# The distinction the whole feature rests on: required checks that exist
# and are PENDING must never be read as "no run exists".
states "OPEN:BLOCKED"
runs_count 0
invoke 431 --interval 1 --timeout 3
assert_rc       "exits 4, not 6"            4
assert_contains "no missing-run claim"      "watch expired"

echo "wait-merged: no REQUIRED check means a missing run blocks nothing"
# A head SHA with no run is only a stall if something was waiting on it.
# With no required check on the base branch the PR merges fine — this
# repository is exactly that case today — so claiming a stall would be a
# false alarm about a PR that is not stuck.
states "OPEN:BLOCKED"
no_checks
runs_count 0
required_checks 0
invoke 431 --interval 1 --timeout 3
assert_rc       "exits 4, not 6"              4
assert_lacks    "makes no missing-run claim"  "no workflow run exists"

echo "wait-merged: a REQUIRED check with no run to report it is a genuine stall"
# Nothing can ever report, so the PR cannot merge. That holds whether a
# webhook was lost or a branch/path filter excluded the required
# workflow — a required check a filter skips waits forever — which is
# why the verdict names both causes and asserts neither.
states "OPEN:BLOCKED"
no_checks
runs_count 0
required_checks 3
invoke 431 --interval 1 --timeout 6
assert_rc       "exits 6"                      6
assert_contains "names the required check"     "REQUIRED"
assert_contains "names the webhook cause"      "webhook"
assert_contains "and the filter cause"         "branch/path filter"
assert_contains "still says to re-arm"         "RE-ARM"

echo "wait-merged: a merge on the empty-checks poll still reports MERGED"
# A PR can merge on the very poll where checks still read empty. The
# terminal state must be honoured BEFORE the stall diagnosis, or a
# successful merge exits 6 and the caller is told not to return to main
# — the one thing this script must never get wrong.
states "OPEN:BLOCKED" "OPEN:BLOCKED" "MERGED:CLEAN"
no_checks
runs_count 0
invoke 431 --interval 1 --timeout 9
assert_rc       "exits 0, not 6"              0
assert_contains "reports the merge"           "MERGED — safe to return to main"

echo
if [[ "$failures" -eq 0 ]]; then
    echo "test_wait-merged: OK — all assertions passed."
    exit 0
fi
echo "test_wait-merged: FAILED — $failures assertion(s) failed." >&2
exit 1
