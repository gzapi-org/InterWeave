#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/gh/test_actions-health.sh
#
# Behavioural tests for actions-health.sh — the hand-off to agent-fabric's
# runtime/github/actions-health.sh. What it decides about GitHub is the
# fabric's and is tested there (tests/test_actions_health_cli.py); what
# this file promises is:
#
#   1. the fabric script runs with the arguments untouched, and its
#      output and exit status are the caller's
#   2. INTERWEAVE_ACTIONS_INCLUDED_MINUTES reaches it as
#      AGENT_FABRIC_ACTIONS_INCLUDED_MINUTES, an explicit value of the
#      fabric's own variable wins, and the setting it names to a person
#      is INTERWEAVE_ACTIONS_INCLUDED_MINUTES
#   3. AGENT_FABRIC_ROOT wins over the sibling-checkout default
#   4. when agent-fabric (or its actions-health) is not there it refuses
#      with exit 2 — "could not find out", never 1, which says GitHub is
#      degraded
#
# The fabric is a sandbox with a recording stub in place of the real
# script. The CI job that runs these suites has no agent-fabric checkout,
# so nothing here may need the real one.
#
# Exit codes:
#   0  all assertions passed
#   1  one or more assertions failed

set -uo pipefail

SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
UNDER_TEST="$SCRIPT_DIR/actions-health.sh"
[[ -f "$UNDER_TEST" ]] || { echo "test: $UNDER_TEST not found" >&2; exit 1; }

failures=0
pass() { echo "  ok   $1"; }
fail() { echo "  FAIL $1"; [[ -n "${2:-}" ]] && printf '%s\n' "$2" | sed 's/^/       /'; failures=$((failures+1)); }

SANDBOX="$(mktemp -d)"
trap 'rm -rf "$SANDBOX"' EXIT

# A fake agent-fabric whose actions-health.sh records argv and the two
# variables it reads.
FABRIC="$SANDBOX/agent-fabric"
mkdir -p "$FABRIC/runtime/github"
STUB="$FABRIC/runtime/github/actions-health.sh"
cat > "$STUB" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$#" > "$RECORD.argc"
printf '%s\0' "$@" > "$RECORD.argv"
printf '%s' "${AGENT_FABRIC_ACTIONS_INCLUDED_MINUTES-<unset>}" > "$RECORD.minutes"
printf '%s' "${AGENT_FABRIC_ACTIONS_INCLUDED_SETTING-<unset>}" > "$RECORD.setting"
echo "stub ran"
exit 1
STUB
chmod +x "$STUB"
run() {  # run <record> [env...] -- [args...]
    local rec="$1"; shift; local envs=()
    while [[ "$1" != -- ]]; do envs+=("$1"); shift; done; shift
    out="$(env -u AGENT_FABRIC_ACTIONS_INCLUDED_MINUTES -u INTERWEAVE_ACTIONS_INCLUDED_MINUTES \
        RECORD="$rec" AGENT_FABRIC_ROOT="$FABRIC" "${envs[@]}" bash "$UNDER_TEST" "$@" 2>&1 </dev/null)"; rc=$?
}

echo "hand-off: arguments reach the fabric script untouched"
run "$SANDBOX/r1" -- --included 'two words' --json
[[ $rc -eq 1 ]] && pass "the fabric script's exit status is returned (1)" || fail "exit status" "rc=$rc out=$out"
[[ "$out" == "stub ran" ]] && pass "its output is the caller's output" || fail "output" "$out"
mapfile -d '' argv < "$SANDBOX/r1.argv"
[[ "$(cat "$SANDBOX/r1.argc")" == 3 && "${argv[0]}" == --included && "${argv[1]}" == "two words" && "${argv[2]}" == --json ]] \
    && pass "three arguments, a space-containing one still one argument" || fail "argv" "$(printf '[%s]' "${argv[@]}")"

echo "the allowance: InterWeave's setting, under the fabric's name"
run "$SANDBOX/r2" INTERWEAVE_ACTIONS_INCLUDED_MINUTES=50000 -- --quiet
[[ "$(cat "$SANDBOX/r2.minutes")" == 50000 ]] && pass "INTERWEAVE_ACTIONS_INCLUDED_MINUTES reaches it as AGENT_FABRIC_ACTIONS_INCLUDED_MINUTES" || fail "mapping" "$(cat "$SANDBOX/r2.minutes")"
[[ "$(cat "$SANDBOX/r2.setting")" == INTERWEAVE_ACTIONS_INCLUDED_MINUTES ]] && pass "and the setting it names to a person is InterWeave's" || fail "setting name" "$(cat "$SANDBOX/r2.setting")"
run "$SANDBOX/r3" INTERWEAVE_ACTIONS_INCLUDED_MINUTES=50000 AGENT_FABRIC_ACTIONS_INCLUDED_MINUTES=3000 -- --quiet
[[ "$(cat "$SANDBOX/r3.minutes")" == 3000 ]] && pass "an explicit AGENT_FABRIC_ACTIONS_INCLUDED_MINUTES wins" || fail "explicit value" "$(cat "$SANDBOX/r3.minutes")"
run "$SANDBOX/r4" -- --quiet
[[ "$(cat "$SANDBOX/r4.minutes")" == "<unset>" && "$(cat "$SANDBOX/r4.setting")" == INTERWEAVE_ACTIONS_INCLUDED_MINUTES ]] \
    && pass "no setting: none is invented, and the setting is still named" || fail "no setting" "$(cat "$SANDBOX/r4.minutes") / $(cat "$SANDBOX/r4.setting")"
run "$SANDBOX/r5" INTERWEAVE_ACTIONS_INCLUDED_MINUTES= -- --quiet
[[ "$(cat "$SANDBOX/r5.minutes")" == "<unset>" ]] && pass "an empty setting is no setting" || fail "empty setting" "$(cat "$SANDBOX/r5.minutes")"

echo "resolution: AGENT_FABRIC_ROOT wins; otherwise the sibling of this working copy"
SIB="$SANDBOX/projects"; mkdir -p "$SIB/interweave/tools/gh" "$SIB/agent-fabric/runtime/github"
cp "$UNDER_TEST" "$SCRIPT_DIR/fabric-root.sh" "$SIB/interweave/tools/gh/"
git -C "$SIB/interweave" init -q 2>/dev/null
printf '#!/usr/bin/env bash\necho "sibling copy"\n' > "$SIB/agent-fabric/runtime/github/actions-health.sh"
out="$(cd "$SIB/interweave" && env -u AGENT_FABRIC_ROOT bash tools/gh/actions-health.sh 2>&1)"
[[ "$out" == "sibling copy" ]] && pass "with no AGENT_FABRIC_ROOT, ../agent-fabric beside the working copy is used" || fail "sibling default" "$out"
out="$(cd "$SIB/interweave" && RECORD="$SANDBOX/r6" AGENT_FABRIC_ROOT="$FABRIC" bash tools/gh/actions-health.sh 2>&1 </dev/null)"
[[ "$out" == "stub ran" ]] && pass "AGENT_FABRIC_ROOT overrides the sibling" || fail "env override" "$out"

echo "the fabric's pinned Python missing (its wrapper's 127) is exit 2"
F127="$SANDBOX/fabric127"; mkdir -p "$F127/runtime/github"
printf '#!/usr/bin/env bash\necho "fabric-python is not installed" >&2\nexit 127\n' > "$F127/runtime/github/actions-health.sh"
out="$(AGENT_FABRIC_ROOT="$F127" bash "$UNDER_TEST" 2>&1 </dev/null)"; rc=$?
[[ $rc -eq 2 ]] && pass "exit 2, not 127" || fail "127 passed through" "rc=$rc"
grep -q "not installed" <<<"$out" && pass "  and the wrapper's message is kept" || fail "  message lost" "$out"

echo "refusal: no agent-fabric (or one without actions-health) is exit 2, never 1"
for where in "$SANDBOX/nowhere" "$SANDBOX/old-fabric"; do
    mkdir -p "$SANDBOX/old-fabric/runtime/github"
    out="$(AGENT_FABRIC_ROOT="$where" bash "$UNDER_TEST" 2>&1 </dev/null)"; rc=$?
    [[ $rc -eq 2 ]] && pass "$(basename "$where"): exit 2" || fail "$(basename "$where"): exit code" "rc=$rc"
    grep -q "agent-fabric not found at $where" <<<"$out" && grep -q AGENT_FABRIC_ROOT <<<"$out" \
        && pass "  names the location it looked in, and how to point elsewhere" || fail "  message" "$out"
done

echo
if (( failures )); then echo "test_actions-health: $failures assertion(s) FAILED"; exit 1; fi
echo "test_actions-health: OK — all assertions passed."
