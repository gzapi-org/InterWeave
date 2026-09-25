#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/gh/test_pr-review-status.sh
#
# Behavioural tests for pr-review-status.sh — the hand-off to agent-fabric's copy (runtime/github/).
# The script itself decides nothing about PRs; what it promises is:
#
#   1. it runs agent-fabric's runtime/github/pr-review-status.sh
#      with the arguments untouched — a space-containing argument (a
#      --session pattern, a PR list) must reach it as ONE argument — and
#      stdin passed through. The real script reads no stdin and takes no
#      body; the stdin assertion is shim hygiene, the same forwarder every
#      tools/gh script here uses (post-review and pr-reply do read a body)
#   2. AGENT_FABRIC_ROOT wins over the sibling-checkout default
#   3. when agent-fabric is not there it refuses with exit 2 and a
#      message naming the expected location — never a silent fallback
#
# The fabric is a sandbox with a recording stub in place of the real
# script, so the assertions are about what was handed over.
#
# Exit codes:
#   0  all assertions passed
#   1  one or more assertions failed

set -uo pipefail

SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
UNDER_TEST="$SCRIPT_DIR/pr-review-status.sh"
[[ -f "$UNDER_TEST" ]] || { echo "test: $UNDER_TEST not found" >&2; exit 1; }

failures=0
pass() { echo "  ok   $1"; }
fail() { echo "  FAIL $1"; [[ -n "${2:-}" ]] && printf '%s\n' "$2" | sed 's/^/       /'; failures=$((failures+1)); }

SANDBOX="$(mktemp -d)"
trap 'rm -rf "$SANDBOX"' EXIT

# A fake agent-fabric whose pr-review-status.sh records argv and stdin verbatim.
FABRIC="$SANDBOX/agent-fabric"
mkdir -p "$FABRIC/runtime/github"
STUB="$FABRIC/runtime/github/pr-review-status.sh"
cat > "$STUB" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$#" > "$RECORD.argc"
printf '%s\0' "$@" > "$RECORD.argv"
cat > "$RECORD.stdin"
echo "stub ran"
exit 7
STUB
chmod +x "$STUB"

echo "hand-off: arguments and stdin reach the fabric script untouched"
RECORD="$SANDBOX/rec1"
body='line with `backticks` and $vars and "quotes"
second line'
out="$(printf '%s' "$body" | RECORD="$RECORD" AGENT_FABRIC_ROOT="$FABRIC" bash "$UNDER_TEST" 'PRRT_x' --flag 'two words' 2>&1)"; rc=$?
[[ $rc -eq 7 ]] && pass "the fabric script's exit status is returned (7)" || fail "exit status" "rc=$rc out=$out"
[[ "$out" == "stub ran" ]] && pass "its output is the caller's output" || fail "output" "$out"
[[ "$(cat "$RECORD.argc")" == 3 ]] && pass "three arguments handed over" || fail "argc" "$(cat "$RECORD.argc")"
mapfile -d '' argv < "$RECORD.argv"
[[ "${argv[0]}" == "PRRT_x" && "${argv[1]}" == "--flag" && "${argv[2]}" == "two words" ]] && pass "arguments intact, a space-containing one still one argument" || fail "argv" "$(printf '[%s]' "${argv[@]}")"
[[ "$(cat "$RECORD.stdin")" == "$body" ]] && pass "stdin passed through byte-for-byte (shim hygiene; this script reads none)" || fail "stdin" "$(cat "$RECORD.stdin")"

echo "resolution: AGENT_FABRIC_ROOT wins; otherwise the sibling of this working copy"
RECORD="$SANDBOX/rec2"
SIB="$SANDBOX/projects"; mkdir -p "$SIB/interweave/tools/gh" "$SIB/agent-fabric/runtime/github"
cp "$UNDER_TEST" "$SIB/interweave/tools/gh/pr-review-status.sh"
git -C "$SIB/interweave" init -q 2>/dev/null
printf '#!/usr/bin/env bash\necho "sibling copy"\n' > "$SIB/agent-fabric/runtime/github/pr-review-status.sh"
out="$(cd "$SIB/interweave" && env -u AGENT_FABRIC_ROOT bash tools/gh/pr-review-status.sh 2>&1)"
[[ "$out" == "sibling copy" ]] && pass "with no AGENT_FABRIC_ROOT, ../agent-fabric beside the working copy is used" || fail "sibling default" "$out"
out="$(cd "$SIB/interweave" && RECORD="$RECORD" AGENT_FABRIC_ROOT="$FABRIC" bash tools/gh/pr-review-status.sh 2>&1 </dev/null)"
[[ "$out" == "stub ran" ]] && pass "AGENT_FABRIC_ROOT overrides the sibling" || fail "env override" "$out"

echo "refusal: no agent-fabric means exit 2 and a message, never a fallback"
out="$(AGENT_FABRIC_ROOT="$SANDBOX/nowhere" bash "$UNDER_TEST" PRRT_x 2>&1 </dev/null)"; rc=$?
[[ $rc -eq 2 ]] && pass "exit 2" || fail "exit code" "rc=$rc"
grep -q "agent-fabric not found at $SANDBOX/nowhere" <<<"$out" && pass "names the location it looked in" || fail "message" "$out"
grep -q "AGENT_FABRIC_ROOT" <<<"$out" && pass "says how to point at a checkout" || fail "hint" "$out"

echo
if (( failures )); then echo "test_pr-review-status: $failures assertion(s) FAILED"; exit 1; fi
echo "test_pr-review-status: OK — all assertions passed."
