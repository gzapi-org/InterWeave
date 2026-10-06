#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/gh/test_actions-health.sh
#
# Behavioural tests for actions-health.sh: the platform status, the
# allowance (this month, private repositories only), and a public
# repository's runs, which no billing figure can hold.
#
# The value of this tool is a decision — spend minutes, or don't — so
# what matters is that each state produces the RIGHT exit code, and in
# particular that "I could not find out" (2) is never mistaken for
# "GitHub is broken" (1). Confusing those would stop work for no reason.
#
# Exit codes:
#   0  all assertions passed
#   1  one or more assertions failed

set -uo pipefail

SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
UNDER_TEST="$SCRIPT_DIR/actions-health.sh"
[[ -f "$UNDER_TEST" ]] || { echo "test: $UNDER_TEST not found" >&2; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "test: jq required" >&2; exit 1; }

failures=0

# An assertion calling a helper this suite does not define is caught by
# tools/checks/run_suite.sh, which CI and `cargo xtask selftests` run
# every suite through (its header has how). Run bare — `bash` on this
# file — such an assertion is skipped silently: run it through the runner.
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
assert_requested() {
    local req; req="$(cat "$SANDBOX/state/last_request" 2>/dev/null || true)"
    if [[ "$req" == *"$2"* ]]; then pass "$1"
    else fail "$1 — request lacked '$2'" "$req"; fi
}

SANDBOX="$(mktemp -d)"
mkdir -p "$SANDBOX/bin" "$SANDBOX/state"

# githubstatus.com, from a fixture. An empty fixture means unreachable.
cat > "$SANDBOX/bin/curl" <<'CURLMOCK'
#!/usr/bin/env bash
set -uo pipefail
[[ -f "$MOCK_STATE/status_unreachable" ]] && exit 7
# REACHED BUT NOT THE SCHEMA. A captive portal answers 200 with an HTML
# login page, so `curl -fsS` succeeds and the body is nonempty — the
# exact shape that made the script claim it had read GitHub's status.
if [[ -f "$MOCK_STATE/status_garbage" ]]; then
  printf '<html><body>Sign in to the network</body></html>\n'; exit 0
fi
status="$(cat "$MOCK_STATE/actions_status" 2>/dev/null || echo operational)"
incident="$(cat "$MOCK_STATE/incident" 2>/dev/null || true)"
printf '{"components":[{"name":"Actions","status":"%s"}],"incidents":[' "$status"
[[ -n "$incident" ]] && printf '{"name":"%s"}' "$incident"
printf ']}\n'
CURLMOCK
chmod +x "$SANDBOX/bin/curl"

# gh: repo owner, this repository's visibility, other repositories'
# visibility, and billing usage — all from fixtures.
cat > "$SANDBOX/bin/gh" <<'GHMOCK'
#!/usr/bin/env bash
set -uo pipefail
if [[ "${1:-}" == "repo" ]]; then
  # THIS repository's visibility: private unless this_repo_public is set;
  # this_repo_unreadable makes the lookup fail as a network error would.
  if [[ " $* " == *" isPrivate "* ]]; then
    [[ -f "$MOCK_STATE/this_repo_unreadable" ]] && exit 1
    [[ -f "$MOCK_STATE/this_repo_public" ]] && { echo false; exit 0; }
    echo true; exit 0
  fi
  echo "testorg"; exit 0
fi
if [[ "${1:-}" == "api" && "${2:-}" == repos/* ]]; then
  # Another repository's visibility: public_repos names the public ones;
  # every other name is private; an unknown one is a 404 (gh prints the
  # error to stdout and exits 1, as the real CLI does).
  name="${2##*/}"
  if grep -qx "$name" "$MOCK_STATE/public_repos" 2>/dev/null; then echo false; exit 0; fi
  if grep -qx "$name" "$MOCK_STATE/unknown_repos" 2>/dev/null; then echo '{"message":"Not Found","status":"404"}'; exit 1; fi
  # A lookup that fails with nothing on stdout (a dropped connection):
  # only the script's own fallback decides what it counts as.
  if grep -qx "$name" "$MOCK_STATE/silent_repos" 2>/dev/null; then exit 1; fi
  echo true; exit 0
fi
if [[ "${1:-}" == "api" ]]; then
  [[ -f "$MOCK_STATE/billing_unreadable" ]] && exit 1
  # The request line, so a test can prove the month is named on it.
  printf '%s\n' "${2:-}" > "$MOCK_STATE/last_request"
  net="$(cat "$MOCK_STATE/billing_net" 2>/dev/null || echo 0)"
  mins="$(cat "$MOCK_STATE/billing_mins" 2>/dev/null || echo 100)"
  # A second Actions line billed in a DIFFERENT unit. Storage is an
  # Actions charge that is not runner minutes, and summing netAmount
  # across the product read it as minute overage.
  stor="$(cat "$MOCK_STATE/billing_storage_net" 2>/dev/null || echo 0)"
  # Rows carry the month they bill for, as the real endpoint's do. The
  # PREVIOUS month's row (billed, as an overage month would be) is what
  # the endpoint hands back beside the current one after a rollover.
  this_month="$(date -u +%Y-%m)-01"
  prev_month="$(date -u -d "$(date -u +%Y-%m-01) -1 day" +%Y-%m)-01"
  prev_mins="$(cat "$MOCK_STATE/billing_prev_mins" 2>/dev/null || echo 0)"
  prev_net="$(cat "$MOCK_STATE/billing_prev_net" 2>/dev/null || echo 0)"
  # A PUBLIC repository's minute row rides beside the private one when
  # billing_public_mins is set: GitHub lists it in the same payload and
  # never counts it toward the included allowance.
  public_mins="$(cat "$MOCK_STATE/billing_public_mins" 2>/dev/null || echo 0)"
  # An older payload's row: neither a date nor a repositoryName. It is
  # counted — the filters exclude other months and public repositories,
  # they do not demand fields a payload may lack.
  bare_mins="$(cat "$MOCK_STATE/billing_bare_mins" 2>/dev/null || echo 0)"
  printf '{"usageItems":[{"product":"actions","sku":"Actions Linux","unitType":"Minutes","quantity":%s,"netAmount":%s,"date":"%s","repositoryName":"privrepo"},{"product":"actions","sku":"Actions Linux","unitType":"Minutes","quantity":%s,"netAmount":%s,"date":"%s","repositoryName":"privrepo"},{"product":"actions","sku":"Actions Linux","unitType":"Minutes","quantity":%s,"netAmount":0,"date":"%s","repositoryName":"openrepo"},{"product":"actions","sku":"Actions Linux","unitType":"Minutes","quantity":'"$bare_mins"',"netAmount":0},{"product":"actions","sku":"Actions Storage","unitType":"GigabyteHours","quantity":10,"netAmount":%s,"date":"%s","repositoryName":"privrepo"}]}\n' \
    "$mins" "$net" "$this_month" "$prev_mins" "$prev_net" "$prev_month" "$public_mins" "$this_month" "$stor" "$this_month"
  exit 0
fi
exit 1
GHMOCK
chmod +x "$SANDBOX/bin/gh"

reset() {
    rm -f "$SANDBOX/state/"*
    printf 'operational\n' > "$SANDBOX/state/actions_status"
    printf '0\n'           > "$SANDBOX/state/billing_net"
    printf '100\n'         > "$SANDBOX/state/billing_mins"
}
# The allowance is read from the environment, so every invocation states
# it explicitly. Inheriting the ambient value would make these tests pass
# or fail depending on the shell that launched them.
invoke() {
    RUN_OUT="$(env -u INTERWEAVE_ACTIONS_INCLUDED_MINUTES \
        PATH="$SANDBOX/bin:$PATH" MOCK_STATE="$SANDBOX/state" \
        bash "$UNDER_TEST" "$@" 2>&1)"
    RUN_RC=$?
}
# invoke_with <allowance> [args...]
invoke_with() {
    local allowance="$1"; shift
    RUN_OUT="$(env PATH="$SANDBOX/bin:$PATH" MOCK_STATE="$SANDBOX/state" \
        INTERWEAVE_ACTIONS_INCLUDED_MINUTES="$allowance" \
        bash "$UNDER_TEST" "$@" 2>&1)"
    RUN_RC=$?
}

echo "actions-health: a healthy platform says go"
reset
invoke
assert_rc       "exits 0"                0
assert_contains "says OK"                "OK — Actions operational"
assert_contains "quotes the minutes"     "100 minutes used"

echo "actions-health: the previous month's minutes are not this period's"
# Unfiltered, the usage endpoint returns last month's rows beside this
# month's after a rollover; summed, two months read as one period, and
# last month's billed overage read as money moving now.
reset
printf '3000\n' > "$SANDBOX/state/billing_mins"
printf '900\n'  > "$SANDBOX/state/billing_prev_mins"
invoke_with 3500
assert_rc        "exits 0 — this month alone is under the allowance" 0
assert_contains  "quotes this month only"         "3000 of 3500 minutes used"
assert_lacks     "never adds the previous month"  "3900 of"
assert_requested "names the year on the request"  "year=$(date -u +%Y)"
assert_requested "names the month on the request" "month=$(date -u +%-m)"
reset
printf '87.384\n' > "$SANDBOX/state/billing_prev_net"
printf '5000\n'   > "$SANDBOX/state/billing_prev_mins"
invoke
assert_rc        "last month's billed overage is not a cost now: exits 0" 0
assert_lacks     "and is not quoted as billing"   "billing as overage"

echo "actions-health: a PUBLIC repository's minutes do not count toward the allowance"
reset
printf '42406\n' > "$SANDBOX/state/billing_mins"
printf '8613\n'  > "$SANDBOX/state/billing_public_mins"
printf 'openrepo\n' > "$SANDBOX/state/public_repos"
invoke_with 50000
assert_rc        "exits 0 — the private repositories are under the allowance" 0
assert_contains  "quotes the private sum only"      "42406 of 50000 minutes used"
assert_lacks     "never adds the public repository" "51019 of"
reset
printf '42406\n' > "$SANDBOX/state/billing_mins"
printf '8613\n'  > "$SANDBOX/state/billing_public_mins"
invoke_with 50000
assert_rc        "the same minutes on a private repository exit 1" 1
assert_contains  "and the sum includes them"        "51019 of 50000"
reset
printf '42406\n' > "$SANDBOX/state/billing_mins"
printf '8613\n'  > "$SANDBOX/state/billing_public_mins"
printf 'openrepo\n' > "$SANDBOX/state/unknown_repos"
invoke_with 50000
assert_rc        "a repository the lookup cannot read (404) is counted" 1
reset
printf '42406\n' > "$SANDBOX/state/billing_mins"
printf '8613\n'  > "$SANDBOX/state/billing_public_mins"
printf 'openrepo\n' > "$SANDBOX/state/silent_repos"
invoke_with 50000
assert_rc        "a lookup that fails with no output is counted too" 1

echo "actions-health: run from a PUBLIC repository, billing cannot hold a run"
# #194 (2026-10-06): "$87 billing as overage, every further minute is
# money" held a reviewed PR whose run billed 0 ms.
reset
touch "$SANDBOX/state/this_repo_public"
printf '87.384\n' > "$SANDBOX/state/billing_net"
printf '60000\n'  > "$SANDBOX/state/billing_mins"
invoke
assert_rc        "billed overage elsewhere, no allowance configured: exits 0" 0
assert_contains  "says the repository is public"    "this repository is public, so its runs bill nothing"
assert_contains  "quotes the private usage as context" "60000 minutes this period, \$87.384 billing as overage"
assert_lacks     "never DEGRADED"                   "DEGRADED"
invoke_with 50000
assert_rc        "past the allowance and billed: still exits 0" 0
printf '0\n' > "$SANDBOX/state/billing_net"
invoke_with 50000
assert_rc        "past the allowance and NOT billed: still exits 0 (no allowance is drawn here)" 0
assert_contains  "and quotes the usage without a cost" "60000 minutes this period.)"
printf 'major_outage\n' > "$SANDBOX/state/actions_status"
invoke
assert_rc        "a degraded platform still stops the work" 1
assert_contains  "and names it"                      "major_outage"
reset
touch "$SANDBOX/state/this_repo_public" "$SANDBOX/state/billing_unreadable"
invoke
assert_rc        "billing unreadable: still answers, exits 0" 0
assert_contains  "and says why it can"               "this repository is public, so its runs bill nothing. (Billing API unreadable.)"
reset
touch "$SANDBOX/state/this_repo_unreadable"
printf '87.384\n' > "$SANDBOX/state/billing_net"
invoke
assert_rc        "a visibility that cannot be read is private: billed exits 1" 1
assert_contains  "with the cost line"                "billing as overage"

echo "actions-health: a row with no date and no repositoryName is counted"
reset
printf '100\n' > "$SANDBOX/state/billing_mins"
printf '500\n' > "$SANDBOX/state/billing_bare_mins"
invoke_with 3000
assert_rc        "exits 0" 0
assert_contains  "and the sum includes it"          "600 of 3000 minutes used"

echo "actions-health: --help prints the whole header"
reset
invoke --help
assert_rc        "exits 0" 0
assert_contains  "down to the exit codes"           "2  invocation problem"
assert_contains  "and the header's last line"       "would stop work for no reason."
assert_lacks     "and no code after it"             "set -uo pipefail"

echo "actions-health: a bad allowance is exit 2 before any network answer"
# major_outage would exit 1 in the status check, so it pins the check
# ahead of the network calls, not merely ahead of billing.
for state in this_repo_public billing_unreadable major_outage; do
    reset
    if [[ "$state" == major_outage ]]; then printf 'major_outage\n' > "$SANDBOX/state/actions_status"
    else touch "$SANDBOX/state/$state"; fi
    invoke_with abc
    assert_rc       "$state: exits 2" 2
    assert_contains "  and names the setting" "must be a positive number"
done

echo "actions-health: a degraded Actions component stops the work"
reset
printf 'major_outage\n' > "$SANDBOX/state/actions_status"
printf 'Incident with Actions\n' > "$SANDBOX/state/incident"
invoke
assert_rc       "exits 1"                1
assert_contains "names the status"       "major_outage"
assert_contains "names the incident"     "Incident with Actions"
assert_contains "says not to spend"      "do not spend minutes"

echo "actions-health: billed overage is a COST, not a block"
# Money moving proves runners are still being SERVED — an organisation
# that purchases overage bills and keeps handing them out. Calling that
# "green code will not merge" halted work on exactly the plan where
# nothing was wrong. It is degraded, because every further minute costs;
# it is not a block, and the message must not say it is.
reset
printf '13.35\n' > "$SANDBOX/state/billing_net"
printf '3200\n'  > "$SANDBOX/state/billing_mins"
invoke
assert_rc       "exits 1 — this is expensive"        1
assert_contains "quotes the usage"                   "3200 minutes used"
assert_contains "names it as overage"                "billing as overage"
assert_contains "and says it blocks nothing"         "blocks nothing"
assert_lacks    "never claims work has stopped"      "will not merge"

# Same, with the allowance configured: past the limit AND billed is the
# expensive case; past the limit and NOT billed is the blocking one.
reset
printf '13.35\n' > "$SANDBOX/state/billing_net"
printf '3200\n'  > "$SANDBOX/state/billing_mins"
invoke_with 3000
assert_rc       "exits 1"                            1
assert_contains "names it as overage"                "billing as overage"
assert_lacks    "and not as a stoppage"              "Nothing will merge"

echo "actions-health: Actions STORAGE is not runner overage"
# `mins` filters on unitType; `net` summed netAmount across the whole
# product. A billed storage line therefore read as "overage is being
# paid for, runners are fine" while minute runners had actually stopped
# — the plan-does-not-buy-overage block, silently inverted.
reset
printf '0\n'     > "$SANDBOX/state/billing_net"
printf '9.99\n'  > "$SANDBOX/state/billing_storage_net"
printf '3025\n'  > "$SANDBOX/state/billing_mins"
invoke_with 3000
assert_rc       "still exits 1"                      1
assert_contains "and names the real cause"           "not buying overage"
assert_lacks    "not the storage charge"             "billing as overage"

echo "actions-health: billed minutes are named even with allowance to spare"
# MONEY CAN MOVE WHILE THE ALLOWANCE HAS ROOM. A separately billable
# runner sku does exactly that. Checking `m >= i` first let this fall
# through to the OK line, so the script told a caller to go ahead
# without mentioning that every further minute costs — the one thing the
# billed semantics exist to say.
reset
printf '7.50\n' > "$SANDBOX/state/billing_net"
printf '100\n'  > "$SANDBOX/state/billing_mins"
invoke_with 3000
assert_rc       "billed under the allowance is still degraded" 1
assert_contains "  and says money is moving"                   "is billing"
assert_contains "  while naming the room that remains"         "100 of 3000"
assert_lacks    "  and does not report plain OK"               "remaining."

echo "actions-health: a spent allowance is caught even when NOTHING is billed"
# The regression that motivated the setting. On 2026-08-07 the org sat at
# 3,025 minutes against a 3,000 allowance with netAmount 0 — a plan that
# does not purchase overage is never billed, GitHub just stops handing out
# runners. The old net-only check reported OK and green-lit a run whose
# jobs then died with no steps. Usage-against-limit is what fires here.
reset
printf '0\n'    > "$SANDBOX/state/billing_net"
printf '3025\n' > "$SANDBOX/state/billing_mins"
invoke_with 3000
assert_rc       "exits 1 despite \$0 billed"  1
assert_contains "names the allowance"         "allowance is spent"
assert_contains "quotes usage AND limit"      "3025 of 3000 minutes used"

echo "actions-health: room left reports the EXACT remainder"
reset
printf '3025\n' > "$SANDBOX/state/billing_mins"
invoke_with 50000
assert_rc       "exits 0"                     0
assert_contains "quotes usage and limit"      "3025 of 50000 minutes used"
assert_contains "quotes what is left"         "46975 remaining"

echo "actions-health: --included overrides the environment"
reset
printf '3025\n' > "$SANDBOX/state/billing_mins"
invoke_with 50000 --included 3000
assert_rc       "flag wins, exits 1"          1
assert_contains "uses the flag's limit"       "3025 of 3000 minutes used"

echo "actions-health: with no allowance configured, it declines to guess"
# Usage alone cannot say what is left. Reporting a remainder here would be
# invention, and reporting DEGRADED would stop work over an unknown.
reset
printf '3025\n' > "$SANDBOX/state/billing_mins"
invoke
assert_rc       "exits 0, not 1"              0
assert_contains "says the remainder is unknown" "Remaining unknown"
assert_contains "names the setting"           "INTERWEAVE_ACTIONS_INCLUDED_MINUTES"

echo "actions-health: a non-numeric allowance is an invocation error"
reset
invoke_with abc
assert_rc       "exits 2, not 0 or 1"         2

echo "actions-health: unknown is NOT the same as broken"
# Neither source readable. Reporting 1 here would halt work over a
# script that simply could not find out — the distinction the exit
# codes exist to preserve.
reset
: > "$SANDBOX/state/status_unreachable"
: > "$SANDBOX/state/billing_unreadable"
invoke
assert_rc       "exits 2, not 1"         2
assert_contains "says health is unknown" "health unknown"

echo "actions-health: reached is not understood"
# A captive portal answers 200 with an HTML login page: `curl -fsS`
# succeeds, the body is nonempty, and nothing in it is GitHub's status.
# Marking the source reachable on the BODY alone meant a script that had
# learned nothing reported "Actions operational" whenever billing was
# also unreadable — the health-unknown case wearing a green answer, from
# the one tool whose job is deciding whether a run is worth spending.
reset
: > "$SANDBOX/state/status_garbage"
: > "$SANDBOX/state/billing_unreadable"
invoke
assert_rc       "an unparseable status is not a readable one" 2
assert_contains "says health is unknown"                      "health unknown"

# The same body with billing READABLE must still not claim a status it
# never read: the allowance answer is real, the Actions verdict is not.
reset
: > "$SANDBOX/state/status_garbage"
invoke
assert_rc       "the allowance answer still stands"    0
assert_contains "but Actions health is not claimed"    "Actions health unread"

echo "actions-health: a readable status with unreadable billing still answers"
reset
: > "$SANDBOX/state/billing_unreadable"
invoke
assert_rc       "exits 0"                0
assert_contains "flags what it skipped"  "Allowance not checked"

echo "actions-health: --quiet prints nothing and still decides"
reset
printf 'major_outage\n' > "$SANDBOX/state/actions_status"
invoke --quiet
assert_rc       "exits 1"                1
if [[ -z "$RUN_OUT" ]]; then pass "prints nothing"
else fail "prints nothing" "$RUN_OUT"; fi

echo "actions-health: invocation errors"
reset
invoke --nope
assert_rc       "unknown option exits 2" 2
invoke --org
assert_rc       "--org needs a value"    2


echo
if [[ "$failures" -eq 0 ]]; then
    echo "test_actions-health: OK — all assertions passed."
    exit 0
fi
echo "test_actions-health: FAILED — $failures assertion(s) failed." >&2
exit 1
