#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/gh/test_pr-sessions.sh
#
# Behavioural tests for pr-sessions.sh.
#
# The script shipped across five PRs (#393, #394, #396, #397, #398)
# with no automated coverage, and review found four defects that a test
# would have caught on the way in — the author comparison that always
# rendered "!", the `/unresolved` filter that reported "none" after a
# FAILED lookup, an argument parser that spun forever on a missing
# operand, and bot branches attributed to a session named after the
# bot. Everything here is driven through the real script with a mocked
# `gh` on PATH, so the assertions are about observable output, not
# internals.
#
# The mock is faithful where it matters: `gh api graphql --jq FILTER`
# applies FILTER with real jq to a fixture GraphQL response, so the
# aggregation the script actually depends on is exercised rather than
# stubbed past.
#
# Run from anywhere:
#   bash tools/gh/test_pr-sessions.sh
#
# Exit codes:
#   0  all assertions passed
#   1  one or more assertions failed

set -uo pipefail

SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
UNDER_TEST="$SCRIPT_DIR/pr-sessions.sh"

if [[ ! -f "$UNDER_TEST" ]]; then
    echo "test: script under test not found at $UNDER_TEST" >&2
    exit 1
fi

command -v jq >/dev/null 2>&1 || {
    echo "test: jq is required to run these tests." >&2; exit 1; }

failures=0
SANDBOX=""

cleanup() { [[ -n "$SANDBOX" && -d "$SANDBOX" ]] && rm -rf "$SANDBOX"; }
trap cleanup EXIT

# ── sandbox ─────────────────────────────────────────────────────────
#
# The script derives "this clone" from `hostname -s` + the git toplevel
# basename, so the sandbox is a real git repo with a known directory
# name and the fixtures are written against that same identity.
CLONE_NAME="interweave-testclone"
HOST="$(hostname -s)"
ME="$HOST/$CLONE_NAME"
OTHER="$HOST/interweave-otherclone"

setup_sandbox() {
    SANDBOX="$(mktemp -d)"
    mkdir -p "$SANDBOX/$CLONE_NAME" "$SANDBOX/bin" "$SANDBOX/fixtures"
    git -C "$SANDBOX/$CLONE_NAME" init -q 2>/dev/null

    cat > "$SANDBOX/bin/gh" <<'MOCK'
#!/usr/bin/env bash
# Mock gh. Behaviour is steered entirely by GH_MOCK_* environment
# variables so each case can pick its own failure mode.
set -uo pipefail

case "${1:-}" in
  pr)
    [[ -n "${GH_MOCK_PRLIST_FAIL:-}" ]] && exit 1
    # Record the --limit actually requested, so a case can assert about
    # the fetch that went out rather than the rendered page. Recording
    # unconditionally is what lets THIS mock serve every case: a second
    # mock installed mid-file to add one behaviour also silently drops
    # the others, and every case after it runs blind.
    args=("$@")
    for ((i = 0; i < ${#args[@]}; i++)); do
      [[ "${args[i]}" == "--limit" ]] && \
        printf '%s\n' "${args[i+1]:-}" > "$GH_MOCK_DIR/last-limit"
    done
    cat "$GH_MOCK_DIR/pr-list.json"
    ;;
  repo)
    [[ -n "${GH_MOCK_REPOVIEW_FAIL:-}" ]] && exit 1
    printf '%s\n' "${GH_MOCK_REPO:-gzapi-org/InterWeave}"
    ;;
  api)
    [[ -n "${GH_MOCK_GRAPHQL_FAIL:-}" ]] && exit 1
    # Apply the caller's --jq filter to the fixture, exactly as gh does.
    filter=""
    while [[ $# -gt 0 ]]; do
      case "$1" in
        --jq) filter="$2"; shift 2 ;;
        *) shift ;;
      esac
    done
    if [[ -n "$filter" ]]; then
      jq -r "$filter" "$GH_MOCK_DIR/graphql.json"
    else
      cat "$GH_MOCK_DIR/graphql.json"
    fi
    ;;
  *)
    echo "mock gh: unhandled '${1:-}'" >&2; exit 1 ;;
esac
MOCK
    chmod +x "$SANDBOX/bin/gh"
}

# Runs the script inside the sandbox clone with the mock on PATH.
# Captures stdout+stderr; sets RUN_OUT and RUN_RC.
run() {
    RUN_OUT="$(cd "$SANDBOX/$CLONE_NAME" && \
        PATH="$SANDBOX/bin:$PATH" GH_MOCK_DIR="$SANDBOX/fixtures" \
        timeout 20 bash "$UNDER_TEST" "$@" 2>&1)"
    RUN_RC=$?
}

# Same, with extra environment (KEY=VALUE ...) before the script args,
# separated by `--`.
run_env() {
    local env=()
    while [[ $# -gt 0 && "$1" != "--" ]]; do env+=("$1"); shift; done
    shift
    RUN_OUT="$(cd "$SANDBOX/$CLONE_NAME" && \
        PATH="$SANDBOX/bin:$PATH" GH_MOCK_DIR="$SANDBOX/fixtures" \
        env "${env[@]}" timeout 20 bash "$UNDER_TEST" "$@" 2>&1)"
    RUN_RC=$?
}

# ── assertions ──────────────────────────────────────────────────────

pass() { echo "  ✓ $1"; }
fail() {
    echo "  ✗ $1" >&2
    printf '%s\n' "${2:-}" | sed 's/^/      /' >&2
    failures=$((failures + 1))
}

assert_rc() {
    local label="$1" want="$2"
    if [[ "$RUN_RC" -eq "$want" ]]; then pass "$label"
    else fail "$label — expected exit $want, got $RUN_RC" "$RUN_OUT"; fi
}

assert_contains() {
    local label="$1" needle="$2"
    if [[ "$RUN_OUT" == *"$needle"* ]]; then pass "$label"
    else fail "$label — output did not contain '$needle'" "$RUN_OUT"; fi
}

assert_not_contains() {
    local label="$1" needle="$2"
    if [[ "$RUN_OUT" != *"$needle"* ]]; then pass "$label"
    else fail "$label — output unexpectedly contained '$needle'" "$RUN_OUT"; fi
}

# ── fixtures ────────────────────────────────────────────────────────

write_pr_list() { printf '%s\n' "$1" > "$SANDBOX/fixtures/pr-list.json"; }
write_graphql() { printf '%s\n' "$1" > "$SANDBOX/fixtures/graphql.json"; }

# Three PRs: two this clone's, one another session's.
default_pr_list() {
    write_pr_list "$(jq -n --arg me "$ME" --arg other "$OTHER" '[
      {number: 30, state: "OPEN",   headRefName: ($me    + "/feat/alpha"),
       title: "alpha", updatedAt: "2026-08-06T10:00:00Z", isDraft: false, mergedAt: null},
      {number: 29, state: "MERGED", headRefName: ($other + "/fix/beta"),
       title: "beta",  updatedAt: "2026-08-05T10:00:00Z", isDraft: false,
       mergedAt: "2026-08-05T11:00:00Z"},
      {number: 28, state: "MERGED", headRefName: ($me    + "/docs/gamma"),
       title: "gamma", updatedAt: "2026-08-04T10:00:00Z", isDraft: false,
       mergedAt: "2026-08-04T11:00:00Z"}
    ]')"
}

# CLAUDE.md §9 puts no vocabulary on <type>. These are the shapes the
# repository actually uses and an allow-list of conventional-commit words
# silently dropped: `stage-4`, `conformance`, `spike-006`.
#
# "Silently" is the defect. The rows did not appear as unconventional,
# they vanished — so `/unresolved` answered "nothing outstanding" while
# nine PRs and an unanswered P1 sat outside the filter.
typed_pr_list() {
    write_pr_list "$(jq -n --arg me "$ME" '[
      {number: 40, state: "MERGED", headRefName: ($me + "/stage-4/libp2p-substrate"),
       title: "substrate", updatedAt: "2026-08-09T10:00:00Z", isDraft: false,
       mergedAt: "2026-08-09T11:00:00Z"},
      {number: 39, state: "MERGED", headRefName: ($me + "/conformance/negative-boundary"),
       title: "conformance", updatedAt: "2026-08-08T10:00:00Z", isDraft: false,
       mergedAt: "2026-08-08T11:00:00Z"},
      {number: 38, state: "OPEN", headRefName: ($me + "/spike-006/identity"),
       title: "spike", updatedAt: "2026-08-07T10:00:00Z", isDraft: false, mergedAt: null},
      {number: 37, state: "OPEN", headRefName: "no-slashes-at-all",
       title: "unattributable", updatedAt: "2026-08-06T10:00:00Z", isDraft: false,
       mergedAt: null}
    ]')"
}

typed_graphql() {
    write_graphql "$(jq -n '{
      data: {repository: {
        p40: {number: 40, author: {login: "andreabenetton"},
              reviewThreads: {nodes: [
                {isResolved: false, comments: {nodes: [{author: {login: "some-reviewer"}}]}}
              ]}},
        p39: {number: 39, author: {login: "andreabenetton"}, reviewThreads: {nodes: []}},
        p38: {number: 38, author: {login: "andreabenetton"}, reviewThreads: {nodes: []}}
      }}
    }')"
}

# #30 — one unresolved thread, reviewer spoke last  → "1!"
# #29 — one unresolved thread, PR AUTHOR spoke last → "1"  (no bang)
# #28 — threads exist but all resolved              → "-"
default_graphql() {
    write_graphql "$(jq -n '{
      data: {repository: {
        p30: {number: 30, author: {login: "andreabenetton"},
              reviewThreads: {nodes: [
                {isResolved: false, comments: {nodes: [{author: {login: "some-reviewer"}}]}}
              ]}},
        p29: {number: 29, author: {login: "andreabenetton"},
              reviewThreads: {nodes: [
                {isResolved: false, comments: {nodes: [{author: {login: "andreabenetton"}}]}}
              ]}},
        p28: {number: 28, author: {login: "andreabenetton"},
              reviewThreads: {nodes: [
                {isResolved: true, comments: {nodes: [{author: {login: "some-reviewer"}}]}}
              ]}}
      }}
    }')"
}

# A PR whose thread page is TRUNCATED: the first 100 are all resolved,
# but hasNextPage says more exist. The count is unknowable from this
# response, so it must read as unknown rather than "none outstanding".
truncated_graphql() {
    write_graphql "$(jq -n '{
      data: {repository: {
        p30: {number: 30, author: {login: "andreabenetton"},
              reviewThreads: {pageInfo: {hasNextPage: true}, nodes: [
                {isResolved: true, comments: {nodes: [{author: {login: "some-reviewer"}}]}}
              ]}}
      }}
    }')"
}

# ── cases ───────────────────────────────────────────────────────────

setup_sandbox

echo "pr-sessions: default scope"
default_pr_list; default_graphql
run
assert_rc        "exits 0" 0
assert_contains  "shows this clone's PR"          "#30"
assert_contains  "shows this clone's other PR"    "#28"
assert_not_contains "hides another session's PR"  "#29"
assert_contains  "says it defaulted to this clone" "Scoped to this clone"

echo "pr-sessions: /all"
run /all
assert_rc       "exits 0" 0
assert_contains "includes the other session's PR" "#29"
assert_contains "still includes this clone's"     "#30"

echo "pr-sessions: explicit session filter"
run --session interweave-otherclone
assert_rc           "exits 0" 0
assert_contains     "matches the named session"     "#29"
assert_not_contains "excludes the other session"    "#30"

echo "pr-sessions: state filter is chosen at fetch time"
run /OPEN
assert_rc       "exits 0" 0

echo "pr-sessions: conflicting state flags are refused"
run /OPEN /MERGED
assert_rc       "exits 2" 2
assert_contains "names the conflict" "conflict"

echo "pr-sessions: grouped output"
run /all --by-session
assert_rc       "exits 0" 0
assert_contains "groups under this clone" "$ME"
assert_contains "marks this clone"        "this clone"

echo "pr-sessions: empty result is a clean exit, not an error"
write_pr_list '[]'
run
assert_rc       "exits 0" 0
assert_contains "says nothing matched" "no PRs"
default_pr_list

echo "pr-sessions: THR column reflects the thread lookup"
run /all
assert_rc       "exits 0" 0
# "1!" — a reviewer spoke last on #30, so a reply is owed.
assert_contains "flags a thread awaiting a reply" "1!"
# #29's unresolved thread was last answered by the PR AUTHOR, so the
# count stands alone: answered, merely not resolved. This is the case
# the broken comparison could not express — `.author` was read off the
# thread node, which has no such field, so every unresolved thread
# compared against null and every row wore a bang.
row_29="$(printf '%s\n' "$RUN_OUT" | grep -- '#29')"
if [[ "$row_29" == *"1!"* ]]; then
    fail "no bang when the author answered last" "$row_29"
else
    pass "no bang when the author answered last"
fi
# #28's threads are all resolved → "-"; if the resolved filter broke,
# this would read "1".
assert_contains "shows resolved-only PRs as a dash" "-"

echo "pr-sessions: --no-threads skips the lookup"
run /all --no-threads
assert_rc           "exits 0" 0
assert_not_contains "no bang without the lookup" "1!"

echo "pr-sessions: /unresolved needs the lookup"
run /unresolved --no-threads
assert_rc       "exits 2" 2
assert_contains "explains the conflict" "drop --no-threads"

echo "pr-sessions: /unresolved keeps only PRs with open threads"
run /all /unresolved
assert_rc           "exits 0" 0
assert_contains     "keeps #30"            "#30"
assert_contains     "keeps #29"            "#29"
assert_not_contains "drops the resolved-only PR" "#28"

echo "pr-sessions: pool filters"
run /all /lastItem:1
assert_rc           "exits 0" 0
assert_contains     "keeps the newest PR"     "#30"
assert_not_contains "drops everything older"  "#29"

run /all /lastDate:99d
assert_rc       "/lastDate accepts <N>d" 0
assert_contains "keeps PRs in the window" "#30"

echo "pr-sessions: malformed pool filters are refused"
run /lastItem:0
assert_rc       "/lastItem:0 exits 2" 2
assert_contains "names the expectation" "positive integer"

run /lastItem:
assert_rc       "empty /lastItem exits 2" 2

run /lastDate:5
assert_rc       "unit-less /lastDate exits 2" 2
assert_contains "names the accepted units" "<N>d"

run /lastDate:
assert_rc       "empty /lastDate exits 2" 2

echo "pr-sessions: unknown options are refused"
run --nope
assert_rc       "exits 2" 2
assert_contains "points at --help" "--help"

echo "pr-sessions: --help lists every documented flag"
run --help
assert_rc       "exits 0" 0
assert_contains "documents /all"        "/all"
assert_contains "documents --by-session" "--by-session"
# These live below the old hard-coded line-36 cut-off. Their absence was
# the discoverability regression review flagged on #394 and #398.
assert_contains "documents /lastItem"   "/lastItem"
assert_contains "documents /lastDate"   "/lastDate"
assert_contains "documents /unresolved" "/unresolved"

echo "pr-sessions: a missing operand is refused, not looped on"
# `-n` last: `shift 2` cannot consume two arguments, and with `set -e`
# off the parser used to spin forever. `timeout` in run() turns a
# regression here into exit 124 rather than a hung suite.
run -n
assert_rc       "bare -n exits 2" 2
assert_contains "names the flag" "-n"

run --session
assert_rc       "bare --session exits 2" 2

echo "pr-sessions: a non-numeric limit is refused before arithmetic"
# `$(( LIMIT * 20 ))` re-evaluates the variable's CONTENT as an
# arithmetic expression, and bash evaluates command substitution inside
# an array subscript — so an unvalidated limit is code execution, not
# just a bad number.
run -n 'x[$(touch "'"$SANDBOX"'/pwned")]'
assert_rc "injected limit exits 2" 2
if [[ -e "$SANDBOX/pwned" ]]; then
    fail "arithmetic injection executed the payload" "$RUN_OUT"
    rm -f "$SANDBOX/pwned"
else
    pass "arithmetic injection did not execute"
fi

run -n abc
assert_rc       "non-numeric limit exits 2" 2
assert_contains "names the expectation" "positive integer"

echo "pr-sessions: bot branches are not attributed to a session"
write_pr_list "$(jq -n --arg me "$ME" '[
  {number: 40, state: "OPEN", headRefName: "dependabot/github_actions/actions-minor-patch-5c7bcdc794",
   title: "bump", updatedAt: "2026-08-06T10:00:00Z", isDraft: false, mergedAt: null},
  {number: 41, state: "OPEN", headRefName: "dependabot/cargo/crates/interweave-core/tokio-minor-patch-04e2",
   title: "bump", updatedAt: "2026-08-06T09:00:00Z", isDraft: false, mergedAt: null},
  {number: 42, state: "OPEN", headRefName: ($me + "/feat/real"),
   title: "real", updatedAt: "2026-08-06T08:00:00Z", isDraft: false, mergedAt: null}
]')"
write_graphql "$(jq -n '{data: {repository: {
  p40: {number: 40, author: {login: "app/dependabot"}, reviewThreads: {nodes: []}},
  p41: {number: 41, author: {login: "app/dependabot"}, reviewThreads: {nodes: []}},
  p42: {number: 42, author: {login: "andreabenetton"}, reviewThreads: {nodes: []}}
}}}')"
run /all
assert_rc       "exits 0" 0
# The branch name itself still shows in the WORK column, so the claim
# has to be about the SESSION column of each bot row specifically.
for n in 40 41; do
    row="$(printf '%s\n' "$RUN_OUT" | grep -- "#$n")"
    if [[ "$row" == *"(unconventional)"* ]]; then
        pass "#$n is not attributed to a session"
    else
        fail "#$n was attributed to a session" "$row"
    fi
done
assert_contains "a real session branch still resolves" "$ME"
default_pr_list; default_graphql

echo "pr-sessions: an invalid --session regex is an invocation error"
# `test()` with a bad pattern kills jq. Swallowing that printed "no
# matching PRs" and exited 0 — the same reassuring answer a genuinely
# empty result gives.
run --session '['
assert_rc       "exits 2" 2
assert_contains "says the filter was rejected" "session"
assert_not_contains "does not claim an empty result" "no PRs for a session"

echo "pr-sessions: a failed thread lookup reads as unknown, never zero"
run_env GH_MOCK_GRAPHQL_FAIL=1 -- /all
assert_rc       "listing still succeeds" 0
assert_contains "THR shows unknown" "?"

run_env GH_MOCK_GRAPHQL_FAIL=1 -- /all /unresolved
assert_rc           "/unresolved refuses to guess" 2
assert_contains     "says the lookup failed"       "could not"
assert_not_contains "never claims there are none"  "no PRs with unresolved review threads"

run_env GH_MOCK_REPOVIEW_FAIL=1 -- /all /unresolved
assert_rc       "/unresolved refuses to guess without a repo" 2

echo "pr-sessions: a failed pr list is an invocation error"
run_env GH_MOCK_PRLIST_FAIL=1 -- /all
assert_rc       "exits 2" 2
assert_contains "names the likely cause" "could not list PRs"

echo "pr-sessions: /lastItem beyond the derived cap is honoured, not clamped"
# The mock records the --limit it was handed, so the claim is about the
# fetch that actually went out rather than the rendered page.
run /all /lastItem:600 --no-threads
assert_rc "exits 0" 0
requested="$(cat "$SANDBOX/fixtures/last-limit" 2>/dev/null || echo missing)"
if [[ "$requested" == "600" ]]; then
    pass "fetches the full 600-PR pool"
else
    fail "silently clamped the pool" "gh pr list --limit was '$requested', wanted 600"
fi

echo "pr-sessions: a full /lastDate page warns that the window may be short"
# The warning fires when the fetch came back full, so the fixture has to
# fill the derived 200-row page.
write_pr_list "$(jq -n --arg me "$ME" '[range(200) | {
  number: (500 - .), state: "OPEN", headRefName: ($me + "/feat/w\(.)"),
  title: "w", updatedAt: "2026-08-06T10:00:00Z", isDraft: false, mergedAt: null}]')"
# -n 10 puts the derived fetch at its 200 floor, which the fixture fills
# exactly.
run /all -n 10 /lastDate:99d --no-threads
assert_rc       "exits 0" 0
assert_contains "warns that the window may be truncated" "may be"
default_pr_list

echo "pr-sessions: a truncated thread page reads as unknown, not as none"
# Past 100 threads only the first page arrives. A PR whose early threads
# are resolved and whose open one sits later would otherwise report "-"
# and be dropped from /unresolved — hiding exactly the outstanding work
# the command promises to surface.
truncated_graphql
run
assert_contains "THR shows unknown, not a dash" "?"

truncated_graphql
run /unresolved
assert_contains "  and /unresolved KEEPS the row" "#30"
default_graphql

echo "pr-sessions: a <type> outside the conventional-commit vocabulary is still a session branch"
typed_pr_list; typed_graphql
run
assert_rc           "exits 0" 0
assert_contains     "shows a stage-N branch"      "#40"
assert_contains     "shows a conformance branch"  "#39"
assert_contains     "shows a spike-NNN branch"    "#38"
assert_contains     "splits session from work"    "stage-4/libp2p-substrate"

echo "pr-sessions: /unresolved finds a thread on a stage-N branch"
typed_pr_list; typed_graphql
run /unresolved
assert_rc           "exits 0" 0
assert_contains     "the stage-N PR is listed"    "#40"
assert_not_contains "the clean one is not"        "#39"

echo "pr-sessions: an unattributable branch is reported, not silently dropped"
typed_pr_list; typed_graphql
run
assert_rc       "exits 0" 0
assert_contains "says how many could not be scoped" "cannot be scoped"
assert_not_contains "and does not list it"          "#37"

echo
if [[ "$failures" -eq 0 ]]; then
    echo "test_pr-sessions: OK — all assertions passed."
    exit 0
fi
echo "test_pr-sessions: FAILED — $failures assertion(s) failed." >&2
exit 1
