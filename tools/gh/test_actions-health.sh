#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/gh/test_actions-health.sh
#
# Behavioural tests for actions-health.sh.
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

# gh: repo owner + billing usage, both from fixtures.
cat > "$SANDBOX/bin/gh" <<'GHMOCK'
#!/usr/bin/env bash
set -uo pipefail
if [[ "${1:-}" == "repo" ]]; then echo "testorg"; exit 0; fi
if [[ "${1:-}" == "api" ]]; then
  [[ -f "$MOCK_STATE/billing_unreadable" ]] && exit 1
  net="$(cat "$MOCK_STATE/billing_net" 2>/dev/null || echo 0)"
  mins="$(cat "$MOCK_STATE/billing_mins" 2>/dev/null || echo 100)"
  # A second Actions line billed in a DIFFERENT unit. Storage is an
  # Actions charge that is not runner minutes, and summing netAmount
  # across the product read it as minute overage.
  stor="$(cat "$MOCK_STATE/billing_storage_net" 2>/dev/null || echo 0)"
  printf '{"usageItems":[{"product":"actions","sku":"Actions Linux","unitType":"Minutes","quantity":%s,"netAmount":%s},{"product":"actions","sku":"Actions Storage","unitType":"GigabyteHours","quantity":10,"netAmount":%s}]}\n' "$mins" "$net" "$stor"
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


# Consulted BEFORE the pass/fail summary: a suite whose assertions never
# ran has not passed, whatever its counter says.
if [[ -s "$GUARD_MARKER" ]]; then
    echo "test_actions-health: FAILED — called $(sort -u "$GUARD_MARKER" | wc -l | tr -d " ") command(s) this suite does not define:" >&2
    sort -u "$GUARD_MARKER" | sed 's/^/      /' >&2
    echo "      Those assertions did not run. Exit 0 would have been a lie." >&2
    rm -f "$GUARD_MARKER"
    exit 1
fi
rm -f "$GUARD_MARKER"
echo
if [[ "$failures" -eq 0 ]]; then
    echo "test_actions-health: OK — all assertions passed."
    exit 0
fi
echo "test_actions-health: FAILED — $failures assertion(s) failed." >&2
exit 1
