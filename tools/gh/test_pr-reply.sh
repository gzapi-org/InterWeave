#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/gh/test_pr-reply.sh
#
# Behavioural tests for pr-reply.sh.
#
# Three things this script promises, each of which is unrecoverable if
# it is wrong — a posted reply cannot be unsent:
#
#   1. the body reaches GitHub byte-for-byte, with no shell expansion
#      (the defect the script exists to remove: a reply lost the name of
#      the guard it described because a backtick was command-substituted
#      on its way through a double-quoted argument)
#   2. another session's PR is refused
#   3. a thread is never resolved unless the reply actually landed
#
# The mock `gh` records the exact argv it was handed, so assertion 1 is
# about what would have been transmitted rather than about what the
# script believed it sent.
#
# Exit codes:
#   0  all assertions passed
#   1  one or more assertions failed

set -uo pipefail

SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
UNDER_TEST="$SCRIPT_DIR/pr-reply.sh"

[[ -f "$UNDER_TEST" ]] || { echo "test: $UNDER_TEST not found" >&2; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "test: jq required" >&2; exit 1; }

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

CLONE_NAME="interweave-testclone"
ME="$(hostname -s)/$CLONE_NAME"
OTHER="$(hostname -s)/interweave-otherclone"

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
assert_not_contains() {
    if [[ "$RUN_OUT" != *"$2"* ]]; then pass "$1"
    else fail "$1 — output unexpectedly had '$2'" "$RUN_OUT"; fi
}

setup_sandbox() {
    SANDBOX="$(mktemp -d)"
    mkdir -p "$SANDBOX/$CLONE_NAME" "$SANDBOX/bin" "$SANDBOX/state"
    git -C "$SANDBOX/$CLONE_NAME" init -q 2>/dev/null

    cat > "$SANDBOX/bin/gh" <<'MOCK'
#!/usr/bin/env bash
# Mock gh: records each graphql call's operation and body, and answers
# from the fixtures the case set up.
set -uo pipefail

query=""; id=""; body=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    -f) case "$2" in
          query=*) query="${2#query=}" ;;
          id=*)    id="${2#id=}" ;;
          body=*)  body="${2#body=}" ;;
        esac
        shift 2 ;;
    *) shift ;;
  esac
done

if [[ "$query" == *"PullRequestReviewThread"* && "$query" != *mutation* ]]; then
  echo "READ $id" >> "$GH_MOCK_STATE/calls"
  [[ -n "${GH_MOCK_READ_FAIL:-}" ]] && exit 1
  cat "$GH_MOCK_STATE/thread.json"
  exit 0
fi

if [[ "$query" == *addPullRequestReviewThreadReply* ]]; then
  echo "REPLY $id" >> "$GH_MOCK_STATE/calls"
  printf '%s' "$body" > "$GH_MOCK_STATE/body.txt"
  [[ -n "${GH_MOCK_REPLY_FAIL:-}" ]] && exit 1
  [[ -n "${GH_MOCK_REPLY_EMPTY:-}" ]] && { echo ""; exit 0; }
  echo "https://github.com/o/r/pull/1#discussion_r1"
  exit 0
fi

if [[ "$query" == *resolveReviewThread* ]]; then
  echo "RESOLVE $id" >> "$GH_MOCK_STATE/calls"
  [[ -n "${GH_MOCK_RESOLVE_FAIL:-}" ]] && exit 1
  echo "true"
  exit 0
fi

echo "mock gh: unhandled query" >&2; exit 1
MOCK
    chmod +x "$SANDBOX/bin/gh"
}

# Writes the thread fixture. $1 = branch, $2 = isResolved.
thread_fixture() {
    jq -n --arg branch "$1" --argjson resolved "$2" '{
      isResolved: $resolved, path: "lib/x.dart", line: 12,
      pullRequest: {number: 77, state: "MERGED", headRefName: $branch},
      comments: {nodes: [{author: {login: "some-reviewer"}}]}
    }' > "$SANDBOX/state/thread.json"
    : > "$SANDBOX/state/calls"
    rm -f "$SANDBOX/state/body.txt"
}

# Runs the script with the given stdin body and arguments.
invoke() {
    local body="$1"; shift
    RUN_OUT="$(cd "$SANDBOX/$CLONE_NAME" && printf '%s' "$body" | \
        env PATH="$SANDBOX/bin:$PATH" GH_MOCK_STATE="$SANDBOX/state" \
        "${MOCK_ENV[@]}" bash "$UNDER_TEST" "$@" 2>&1)"
    RUN_RC=$?
}

MOCK_ENV=(env)

calls() { cat "$SANDBOX/state/calls" 2>/dev/null; }

setup_sandbox
THREAD_ID="PRRT_kwDOtest123"

echo "pr-reply: the happy path"
thread_fixture "$ME/feat/thing" false
invoke "Fixed in #123." "$THREAD_ID"
assert_rc       "exits 0" 0
assert_contains "reports the reply URL" "discussion_r1"
assert_contains "reports the resolve"   "resolved: #77"
if [[ "$(calls)" == "READ $THREAD_ID
REPLY $THREAD_ID
RESOLVE $THREAD_ID" ]]; then
    pass "read, then reply, then resolve — in that order"
else
    fail "call order wrong" "$(calls)"
fi

echo "pr-reply: the body is transmitted verbatim"
# The defect this script exists to remove. Every one of these is
# something the shell would have eaten from a double-quoted argument.
BODY='Fixed: the `need_operand` guard and $LIMIT — see $(date) and "quotes" \back.'
thread_fixture "$ME/feat/thing" false
invoke "$BODY" "$THREAD_ID"
assert_rc "exits 0" 0
sent="$(cat "$SANDBOX/state/body.txt" 2>/dev/null || echo MISSING)"
if [[ "$sent" == "$BODY" ]]; then
    pass "backticks, \$vars, \$( ), quotes and backslashes survive intact"
else
    fail "the body was mangled in transit" "sent: $sent
want: $BODY"
fi

echo "pr-reply: --no-resolve replies without claiming the finding is handled"
thread_fixture "$ME/feat/thing" false
invoke "Real, but the contract has to change first." "$THREAD_ID" --no-resolve
assert_rc           "exits 0" 0
assert_contains     "says it left the thread open" "left open"
assert_not_contains "did not resolve"              "resolved: #77"
if [[ "$(calls)" != *RESOLVE* ]]; then
    pass "no resolve mutation was sent"
else
    fail "resolved despite --no-resolve" "$(calls)"
fi

echo "pr-reply: another session's PR is refused"
thread_fixture "$OTHER/fix/theirs" false
invoke "I would like to answer this." "$THREAD_ID"
assert_rc       "exits 2" 2
assert_contains "names the owning session" "$OTHER"
assert_contains "explains why not"         "cannot be unsent"
if [[ "$(calls)" != *REPLY* ]]; then
    pass "nothing was posted"
else
    fail "posted to another session's PR" "$(calls)"
fi

echo "pr-reply: a failed reply never resolves the thread"
# The worst available outcome is a resolved thread with no reply: it
# reads as answered and shows nothing.
thread_fixture "$ME/feat/thing" false
MOCK_ENV=(env GH_MOCK_REPLY_FAIL=1)
invoke "…" "$THREAD_ID"
MOCK_ENV=(env)
assert_rc       "exits 2" 2
assert_contains "says the reply was rejected" "rejected"
if [[ "$(calls)" != *RESOLVE* ]]; then
    pass "the thread was left open"
else
    fail "resolved after a failed reply" "$(calls)"
fi

echo "pr-reply: a reply with no URL is treated as not posted"
thread_fixture "$ME/feat/thing" false
MOCK_ENV=(env GH_MOCK_REPLY_EMPTY=1)
invoke "…" "$THREAD_ID"
MOCK_ENV=(env)
assert_rc       "exits 2" 2
assert_contains "says so plainly" "did not post"

echo "pr-reply: a failed resolve is reported, not swallowed"
thread_fixture "$ME/feat/thing" false
MOCK_ENV=(env GH_MOCK_RESOLVE_FAIL=1)
invoke "Fixed." "$THREAD_ID"
MOCK_ENV=(env)
assert_rc       "exits 2" 2
assert_contains "says the reply landed"      "replied:"
assert_contains "and that the thread is open" "still open"

echo "pr-reply: an already-resolved thread is not re-resolved"
thread_fixture "$ME/feat/thing" true
invoke "One more note." "$THREAD_ID"
assert_rc       "exits 0" 0
assert_contains "says it is already resolved" "already resolved"
if [[ "$(calls)" != *RESOLVE* ]]; then
    pass "no resolve mutation was sent"
else
    fail "re-resolved" "$(calls)"
fi

echo "pr-reply: invocation errors"
thread_fixture "$ME/feat/thing" false
invoke "body" "PRRC_notathread"
assert_rc       "a comment id is refused" 2
assert_contains "explains the difference" "PRRC_ is a comment"

invoke "body" "not-an-id"
assert_rc       "a malformed id is refused" 2

invoke "" "$THREAD_ID"
assert_rc       "an empty body is refused" 2
assert_contains "says the body is empty" "empty"

invoke "   " "$THREAD_ID"
assert_rc       "a whitespace-only body is refused" 2

invoke "body"
assert_rc       "a missing thread id is refused" 2

invoke "body" "$THREAD_ID" "PRRT_second"
assert_rc       "two thread ids are refused" 2

invoke "body" "$THREAD_ID" --nope
assert_rc       "an unknown option is refused" 2

echo "pr-reply: a failed thread read stops before posting"
thread_fixture "$ME/feat/thing" false
MOCK_ENV=(env GH_MOCK_READ_FAIL=1)
invoke "body" "$THREAD_ID"
MOCK_ENV=(env)
assert_rc "exits 2" 2
if [[ "$(calls)" != *REPLY* ]]; then
    pass "nothing was posted"
else
    fail "posted without knowing the owner" "$(calls)"
fi

echo "pr-reply: --dry-run touches nothing"
thread_fixture "$ME/feat/thing" false
invoke "Would say this." "$THREAD_ID" --dry-run
assert_rc       "exits 0" 0
assert_contains "shows the target"  "#77"
assert_contains "shows the body"    "Would say this."
if [[ "$(calls)" != *REPLY* && "$(calls)" != *RESOLVE* ]]; then
    pass "no mutation was sent"
else
    fail "--dry-run mutated something" "$(calls)"
fi

echo "pr-reply: --help lists the flags"
invoke "" --help
assert_rc       "exits 0" 0
assert_contains "documents --no-resolve" "--no-resolve"
assert_contains "documents --dry-run"    "--dry-run"


# Consulted BEFORE the pass/fail summary: a suite whose assertions never
# ran has not passed, whatever its counter says.
if [[ -s "$GUARD_MARKER" ]]; then
    echo "test_pr-reply: FAILED — called $(sort -u "$GUARD_MARKER" | wc -l | tr -d " ") command(s) this suite does not define:" >&2
    sort -u "$GUARD_MARKER" | sed 's/^/      /' >&2
    echo "      Those assertions did not run. Exit 0 would have been a lie." >&2
    rm -f "$GUARD_MARKER"
    exit 1
fi
rm -f "$GUARD_MARKER"
echo
if [[ "$failures" -eq 0 ]]; then
    echo "test_pr-reply: OK — all assertions passed."
    exit 0
fi
echo "test_pr-reply: FAILED — $failures assertion(s) failed." >&2
exit 1
