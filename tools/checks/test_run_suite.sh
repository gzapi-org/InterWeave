#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/test_run_suite.sh
#
# Behavioural tests for run_suite.sh.
#
# The load-bearing case is the first: a suite that calls an undefined
# helper, run for real, exits 0 announcing "all assertions passed". That
# is the silence run_suite exists to break, and this file proves it is
# real rather than asserting it from run_suite's own message.
#
# Every fixture is driven THROUGH run_suite rather than directly, which
# also exercises the nesting: the inner run exports its own marker, so a
# deliberately-broken fixture is attributed to the inner run and does not
# contaminate this suite when CI runs it under run_suite too.
#
# Exit codes:
#   0  all assertions passed
#   1  one or more assertions failed

set -uo pipefail

SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
UNDER_TEST="$SCRIPT_DIR/run_suite.sh"

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

write() { cat > "$SANDBOX/$1"; chmod +x "$SANDBOX/$1"; }
run()   { RUN_OUT="$(bash "$UNDER_TEST" "$SANDBOX/$1" 2>&1)"; RUN_RC=$?; }

echo "run_suite: an undefined helper fails the suite"
write broken.sh <<'SUITE'
#!/usr/bin/env bash
set -uo pipefail
failures=0
pass() { echo "  ok $1"; }
fail() { failures=$((failures + 1)); }
assert_rc() { pass "$1"; }
assert_rc    "a real assertion"
assert_lacks "a negative assertion that does not exist" "needle"
if [[ "$failures" -eq 0 ]]; then echo "OK — all assertions passed."; exit 0; fi
exit 1
SUITE

# First: the silence is real. Run it WITHOUT the wrapper — and with the
# handler explicitly removed from the environment, because CI runs THIS
# suite under run_suite too, and an exported handler would otherwise
# catch the fixture's undefined call and attribute it here. Bash exports
# functions as BASH_FUNC_<name>%%, so dropping that variable gives a
# genuinely unprotected child, which is the whole point of the case.
RAW_OUT="$(env -u "BASH_FUNC_command_not_found_handle%%" bash "$SANDBOX/broken.sh" 2>&1)"; RAW_RC=$?
if (( RAW_RC == 0 )); then pass "unwrapped, the broken suite exits 0"
else fail "unwrapped, the broken suite exits 0" "got $RAW_RC"; fi
if [[ "$RAW_OUT" == *"all assertions passed"* ]]; then
    pass "  and claims every assertion passed"
else fail "  and claims every assertion passed" "$RAW_OUT"; fi

run broken.sh
assert_rc       "wrapped, it exits 1"            1
assert_contains "names the undefined command"    "assert_lacks"
assert_contains "explains why the suite missed it" "failures counter never saw them"

echo "run_suite: a sound suite passes through untouched"
write sound.sh <<'SUITE'
#!/usr/bin/env bash
set -uo pipefail
assert_rc()    { :; }
assert_lacks() { :; }
assert_rc "ok"
assert_lacks "ok" "x"
echo "OK — all assertions passed."
exit 0
SUITE
run sound.sh
assert_rc       "exits 0"                        0
assert_contains "its own output survives"        "all assertions passed"
assert_lacks    "and nothing is reported"        "undefined command"

echo "run_suite: a suite's own failure is preserved, not masked"
write failing.sh <<'SUITE'
#!/usr/bin/env bash
echo "  ✗ a real assertion failed"
exit 1
SUITE
run failing.sh
assert_rc       "exit 1 passes through"          1
assert_lacks    "not reported as undefined"      "do not exist"

echo "run_suite: an exit code other than 0/1 survives"
write exit9.sh <<'SUITE'
#!/usr/bin/env bash
exit 9
SUITE
run exit9.sh
assert_rc       "exit 9 passes through"          9

echo "run_suite: undefined commands in a CHILD shell are caught too"
# The handler is exported, so a suite that shells out is covered — which
# is where a mistyped tool name would otherwise hide.
write child.sh <<'SUITE'
#!/usr/bin/env bash
bash -c 'definitely_not_a_real_command_xyz'
echo "OK — all assertions passed."
exit 0
SUITE
run child.sh
assert_rc       "exits 1"                        1
assert_contains "names the child's command"      "definitely_not_a_real_command_xyz"

echo "run_suite: it is not fooled by comments, heredocs or quotes"
# The whole reason for this shape. A static reader had to be taught each
# of these in turn and got each wrong once; nothing is parsed here, so
# none of them can produce a false failure.
write lexical.sh <<'SUITE'
#!/usr/bin/env bash
assert_rc() { :; }
# A future assert_missing helper would go here.
cat > /dev/null <<'FIXTURE'
assert_inside_a_heredoc "not a call"
FIXTURE
assert_rc "real" # don't invoke assert_future — an apostrophe, deliberately
echo "OK — all assertions passed."
exit 0
SUITE
run lexical.sh
assert_rc       "no false failure"               0
assert_lacks    "nothing reported"               "do not exist"

echo "run_suite: usage errors are exit 2"
RUN_OUT="$(bash "$UNDER_TEST" 2>&1)"; RUN_RC=$?
assert_rc       "no argument exits 2"            2
RUN_OUT="$(bash "$UNDER_TEST" "$SANDBOX/nope.sh" 2>&1)"; RUN_RC=$?
assert_rc       "unreadable suite exits 2"       2

# The hand-over to agent-fabric's run-suite.sh: every case above ran the
# real one (CI points AGENT_FABRIC_ROOT at its pinned checkout); these pin
# what the hand-over itself promises, against a recording stub.
hcheck() { if eval "$2"; then pass "$1"; else fail "$1" "$hout"; fi; }
hstub="$(mktemp -d)"
mkdir -p "$hstub/fabric/runtime/github" "$hstub/fabric/projects/interweave/integration/gh"
cat > "$hstub/fabric/runtime/github/run-suite.sh" <<'STUB'
#!/usr/bin/env bash
printf 'config=%s cache=%s' "${NONE-<unset>}" "${AGENT_FABRIC_TOOL_CACHE-<unset>}"; printf ' [%s]' "$@"; echo
exit 3
STUB
# shellcheck disable=SC2034 # read in hcheck's eval'd conditions
hrun() { hout="$(env -u AGENT_FABRIC_NONE -u AGENT_FABRIC_TOOL_CACHE -u INTERWEAVE_TOOL_CACHE AGENT_FABRIC_ROOT="$hstub/fabric" "$@" 2>&1)"; hrc=$?; }
# shellcheck disable=SC2034 # read in hcheck's eval'd conditions
hrepo="$( cd -- "$SCRIPT_DIR/../.." && pwd )"
hrun bash "$UNDER_TEST" a 'two words'
hcheck "the fabric's runner gets the arguments untouched, and its exit is ours" '[[ $hrc -eq 3 && "$hout" == *" [a] [two words]" ]]'
hout="$(AGENT_FABRIC_ROOT="$hstub/none" bash "$UNDER_TEST" x 2>&1)"
# shellcheck disable=SC2034 # read in hcheck's eval'd conditions
hrc=$?
hcheck "no agent-fabric: exit 2, naming where it looked" '[[ $hrc -eq 2 && "$hout" == *"agent-fabric not found at $hstub/none"* ]]'
rm -rf "$hstub"

echo
if [[ "$failures" -eq 0 ]]; then
    echo "test_run_suite: OK — all assertions passed."
    exit 0
fi
echo "test_run_suite: FAILED — $failures assertion(s) failed." >&2
exit 1
