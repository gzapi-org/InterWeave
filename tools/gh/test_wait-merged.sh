#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/gh/test_wait-merged.sh
#
# Behavioural tests for wait-merged.sh — the hand-off to agent-fabric's
# `bin/fabric-pr wait-merged`. Every verdict, and how GitHub is read
# for it, is the fabric's and is tested there (tests/test_wait_merged.py,
# which this file's former cases were the oracle for); what this file
# promises is:
#
#   1. the fabric script runs with the arguments untouched, and its
#      output and exit status are the caller's (every code in the table)
#   2. InterWeave's arm.json is named, so the arm command its lines suggest is
#      tools/gh/arm.sh; an explicit AGENT_FABRIC_ARM_CONFIG wins
#   3. INTERWEAVE_ACTIONS_INCLUDED_MINUTES reaches it under the fabric's
#      name, an explicit fabric value winning, and the allowance setting
#      it names to a person is InterWeave's
#   4. AGENT_FABRIC_ROOT wins over the sibling-checkout default
#   5. when agent-fabric (or its wait-merged, or InterWeave's arm.json) is not
#      there it refuses with exit 2, naming where it looked
#   6. it execs: the fabric's script IS the process a background watch
#      holds, so stopping the watch stops it
#
# The fabric is a sandbox with a recording stub in place of the real
# script, so nothing here reads GitHub or needs the real checkout.
#
# Exit codes:
#   0  all assertions passed
#   1  one or more assertions failed

set -uo pipefail

SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
UNDER_TEST="$SCRIPT_DIR/wait-merged.sh"
[[ -f "$UNDER_TEST" ]] || { echo "test: $UNDER_TEST not found" >&2; exit 1; }

failures=0
pass() { echo "  ok   $1"; }
fail() { echo "  FAIL $1"; [[ -n "${2:-}" ]] && printf '%s\n' "$2" | sed 's/^/       /'; failures=$((failures+1)); }

SANDBOX="$(mktemp -d)"
trap 'rm -rf "$SANDBOX"' EXIT

FABRIC="$SANDBOX/agent-fabric"
RULES="$FABRIC/projects/interweave/integration/gh/arm.json"
mkdir -p "$FABRIC/bin" "${RULES%/*}"
printf '{}' > "$RULES"
cat > "$FABRIC/bin/fabric-pr" <<'STUB'
#!/usr/bin/env bash
# The fabric's one command: the forwarder must name the verb first.
[[ "$1" == wait-merged ]] || { echo "stub fabric-pr: verb '$1', not wait-merged" >&2; exit 99; }
shift
printf '%s\0' "$@" > "$RECORD.argv"
for v in AGENT_FABRIC_ARM_CONFIG AGENT_FABRIC_ACTIONS_INCLUDED_MINUTES AGENT_FABRIC_ACTIONS_INCLUDED_SETTING AGENT_FABRIC_IDLE_READS_BEFORE_STALL; do
    printf '%s=%s\n' "$v" "${!v-<unset>}"
done > "$RECORD.env"
printf '%s' "$PPID" > "$RECORD.ppid"
echo "PR #${1:-?} stub verdict (exit ${STUB_RC:-0})"
exit "${STUB_RC:-0}"
STUB
CLEAN_ENV=(-u AGENT_FABRIC_ARM_CONFIG -u AGENT_FABRIC_ACTIONS_INCLUDED_MINUTES -u AGENT_FABRIC_ACTIONS_INCLUDED_SETTING
           -u AGENT_FABRIC_IDLE_READS_BEFORE_STALL -u INTERWEAVE_ACTIONS_INCLUDED_MINUTES)
run() {  # run <record> [env...] -- [args...]
    local rec="$1"; shift; local envs=()
    while [[ "$1" != -- ]]; do envs+=("$1"); shift; done; shift
    out="$(env "${CLEAN_ENV[@]}" RECORD="$rec" AGENT_FABRIC_ROOT="$FABRIC" "${envs[@]}" bash "$UNDER_TEST" "$@" 2>&1 </dev/null)"; rc=$?
}
envof() { sed -n "s/^$2=//p" "$1.env"; }

echo "hand-off: arguments reach the fabric script untouched; its exit is the caller's"
run "$SANDBOX/r1" -- 431 --interval 10s --timeout '1 h'
mapfile -d '' argv < "$SANDBOX/r1.argv"
[[ ${#argv[@]} -eq 5 && "${argv[0]}" == 431 && "${argv[3]}" == --timeout && "${argv[4]}" == "1 h" ]] \
    && pass "five arguments, a space-containing one still one argument" || fail "argv" "$(printf '[%s]' "${argv[@]}")"
[[ "$out" == "PR #431 stub verdict (exit 0)" ]] && pass "its one stdout line is the caller's" || fail "output" "$out"
for code in 0 2 3 4 5 6 127; do
    run "$SANDBOX/rc$code" STUB_RC=$code -- 431
    [[ $rc -eq $code ]] && pass "exit $code passes through" || fail "exit $code" "rc=$rc out=$out"
done

echo "InterWeave's settings, under the fabric's names"
[[ "$(envof "$SANDBOX/r1" AGENT_FABRIC_ARM_CONFIG)" == "$RULES" ]] && pass "InterWeave's arm.json is named" || fail "arm config" "$(cat "$SANDBOX/r1.env")"
[[ "$(envof "$SANDBOX/r1" AGENT_FABRIC_ACTIONS_INCLUDED_MINUTES)" == "<unset>" ]] \
    && pass "no setting: none is invented" || fail "invented settings" "$(cat "$SANDBOX/r1.env")"
[[ "$(envof "$SANDBOX/r1" AGENT_FABRIC_ACTIONS_INCLUDED_SETTING)" == INTERWEAVE_ACTIONS_INCLUDED_MINUTES ]] \
    && pass "the allowance setting it names to a person is InterWeave's" || fail "setting name" "$(cat "$SANDBOX/r1.env")"
run "$SANDBOX/r2" INTERWEAVE_ACTIONS_INCLUDED_MINUTES=50000 -- 1
[[ "$(envof "$SANDBOX/r2" AGENT_FABRIC_ACTIONS_INCLUDED_MINUTES)" == 50000 ]] && pass "INTERWEAVE_ACTIONS_INCLUDED_MINUTES reaches it" || fail "minutes mapping" "$(cat "$SANDBOX/r2.env")"
run "$SANDBOX/r3" INTERWEAVE_ACTIONS_INCLUDED_MINUTES=50000 AGENT_FABRIC_ACTIONS_INCLUDED_MINUTES=3000 \
    AGENT_FABRIC_ARM_CONFIG=/elsewhere.json -- 1
[[ "$(envof "$SANDBOX/r3" AGENT_FABRIC_ACTIONS_INCLUDED_MINUTES)" == 3000 \
   && "$(envof "$SANDBOX/r3" AGENT_FABRIC_ARM_CONFIG)" == /elsewhere.json ]] \
    && pass "explicit fabric values win" || fail "precedence" "$(cat "$SANDBOX/r3.env")"
run "$SANDBOX/r4" INTERWEAVE_ACTIONS_INCLUDED_MINUTES= -- 1
[[ "$(envof "$SANDBOX/r4" AGENT_FABRIC_ACTIONS_INCLUDED_MINUTES)" == "<unset>" ]] && pass "an empty setting is no setting" || fail "empty setting" "$(cat "$SANDBOX/r4.env")"

echo "it execs: the fabric's script is the watched process"
out="$(env "${CLEAN_ENV[@]}" RECORD="$SANDBOX/r5" AGENT_FABRIC_ROOT="$FABRIC" bash -c 'bash "$1" 9 >/dev/null; echo "$$"' _ "$UNDER_TEST")"
[[ "$(cat "$SANDBOX/r5.ppid")" == "$out" ]] && pass "the stub's parent is the caller's shell, not a forwarder left behind" \
    || fail "not exec'd" "stub parent $(cat "$SANDBOX/r5.ppid"), caller $out"

echo "resolution: AGENT_FABRIC_ROOT wins; otherwise the sibling of this working copy"
SIB="$SANDBOX/projects"; mkdir -p "$SIB/interweave/tools/gh" "$SIB/agent-fabric/bin" "$SIB/agent-fabric/projects/interweave/integration/gh"
cp "$UNDER_TEST" "$SCRIPT_DIR/fabric-root.sh" "$SIB/interweave/tools/gh/"
git -C "$SIB/interweave" init -q 2>/dev/null
printf '{}' > "$SIB/agent-fabric/projects/interweave/integration/gh/arm.json"
printf '#!/usr/bin/env bash\necho "sibling copy"\n' > "$SIB/agent-fabric/bin/fabric-pr"
out="$(cd "$SIB/interweave" && env -u AGENT_FABRIC_ROOT bash tools/gh/wait-merged.sh 1 2>&1)"
[[ "$out" == "sibling copy" ]] && pass "with no AGENT_FABRIC_ROOT, ../agent-fabric beside the working copy is used" || fail "sibling default" "$out"

echo "refusal: no agent-fabric, one without wait-merged, or without InterWeave's arm.json, is exit 2"
mkdir -p "$SANDBOX/old-fabric/runtime/github" "$SANDBOX/no-rules/bin"
cp "$FABRIC/bin/fabric-pr" "$SANDBOX/no-rules/bin/"
for where in "$SANDBOX/nowhere" "$SANDBOX/old-fabric" "$SANDBOX/no-rules"; do
    out="$(AGENT_FABRIC_ROOT="$where" bash "$UNDER_TEST" 1 2>&1 </dev/null)"; rc=$?
    [[ $rc -eq 2 ]] && pass "$(basename "$where"): exit 2" || fail "$(basename "$where"): exit code" "rc=$rc"
    grep -q "agent-fabric not found at $where" <<<"$out" && grep -q AGENT_FABRIC_ROOT <<<"$out" \
        && pass "  names the location it looked in, and how to point elsewhere" || fail "  message" "$out"
done

echo
if (( failures )); then echo "test_wait-merged: $failures assertion(s) FAILED"; exit 1; fi
echo "test_wait-merged: OK — all assertions passed."
