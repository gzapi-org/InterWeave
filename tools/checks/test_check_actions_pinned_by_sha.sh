#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/test_check_actions_pinned_by_sha.sh
#
# Behavioural tests for check_actions_pinned_by_sha.sh, the hand-off to
# agent-fabric's policies/check_actions_pinned_by_sha.py. What the check
# decides is the fabric's and is tested there; what this file promises:
#   1. the fabric's check runs on the pinned Python with the arguments
#      untouched, and its output and exit status are the caller's
#   2. with no --root given, --root is this working tree's top level; a
#      given --root (or --root=) is the caller's and is not doubled
#   3. no agent-fabric, or no pinned Python, is exit 2 and says which
# The fabric and the Python are stubs that record what they were given.
#
# Exit codes: 0 all assertions passed; 1 otherwise.
set -uo pipefail
SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
UNDER_TEST="$SCRIPT_DIR/check_actions_pinned_by_sha.sh"
failures=0
pass() { echo "  ok   $1"; }
fail() { echo "  FAIL $1"; [[ -n "${2:-}" ]] && printf '%s\n' "$2" | sed 's/^/       /'; failures=$((failures+1)); }
SANDBOX="$(realpath -- "$(mktemp -d)")"; trap 'rm -rf "$SANDBOX"' EXIT

# A working copy with the forwarder and its lookup, a stub fabric beside
# it whose check records argv, and a stub Python that runs it with bash.
P="$SANDBOX/projects"; C="$P/interweave"
mkdir -p "$C/tools/checks" "$C/tools/gh" "$P/agent-fabric/policies" "$SANDBOX/bin"
cp "$UNDER_TEST" "$C/tools/checks/"; cp "$SCRIPT_DIR/../gh/fabric-root.sh" "$C/tools/gh/"
git -C "$C" init -q
cat > "$P/agent-fabric/policies/check_actions_pinned_by_sha.py" <<'STUB'
printf '%s\0' "$@" > "$RECORD"
echo "stub check ran"
exit "${STUB_RC:-0}"
STUB
printf '#!/usr/bin/env bash\nscript="$1"; shift; exec bash "$script" "$@"\n' > "$SANDBOX/bin/py"; chmod +x "$SANDBOX/bin/py"
run() { out="$(cd "$C" && env -u AGENT_FABRIC_ROOT AGENT_FABRIC_PYTHON="$SANDBOX/bin/py" RECORD="$SANDBOX/rec" "$@" 2>&1)"; rc=$?; }
argv() { local a=(); mapfile -d '' a < "$SANDBOX/rec"; printf '[%s]' "${a[@]}"; }

echo "hand-off: the fabric's check, on the pinned Python, with the arguments"
run bash tools/checks/check_actions_pinned_by_sha.sh
[[ $rc -eq 0 && "$out" == "stub check ran" ]] && pass "its output and exit status are the caller's" || fail "output or status" "rc=$rc $out"
[[ "$(argv)" == "[--root][$C]" ]] && pass "no --root given: --root is the working tree's top level" || fail "default root" "$(argv)"
STUB_RC=1 run bash tools/checks/check_actions_pinned_by_sha.sh
[[ $rc -eq 1 ]] && pass "a finding (exit 1) is the caller's exit 1" || fail "exit 1 not passed through" "rc=$rc"
run bash tools/checks/check_actions_pinned_by_sha.sh --root /elsewhere
[[ "$(argv)" == "[--root][/elsewhere]" ]] && pass "a given --root is the caller's, not doubled" || fail "explicit --root" "$(argv)"
run bash tools/checks/check_actions_pinned_by_sha.sh --root=/elsewhere
[[ "$(argv)" == "[--root=/elsewhere]" ]] && pass "  --root= too" || fail "--root=" "$(argv)"
run bash tools/checks/check_actions_pinned_by_sha.sh --help
[[ "$(argv)" == "[--root][$C][--help]" ]] && pass "--help reaches the check" || fail "--help" "$(argv)"

echo "refusal: no agent-fabric, or no pinned Python, is exit 2"
out="$(cd "$C" && AGENT_FABRIC_ROOT="$SANDBOX/nowhere" AGENT_FABRIC_PYTHON="$SANDBOX/bin/py" bash tools/checks/check_actions_pinned_by_sha.sh 2>&1)"; rc=$?
[[ $rc -eq 2 && "$out" == *"agent-fabric not found at $SANDBOX/nowhere"* ]] && pass "no agent-fabric: exit 2, the place it looked named" || fail "no fabric" "rc=$rc $out"
out="$(cd "$C" && env -u AGENT_FABRIC_ROOT AGENT_FABRIC_PYTHON="$SANDBOX/no-python" bash tools/checks/check_actions_pinned_by_sha.sh 2>&1)"; rc=$?
[[ $rc -eq 2 && "$out" == *"pinned Python is not installed at $SANDBOX/no-python"* ]] && pass "no pinned Python: exit 2, how to install it named" || fail "no python" "rc=$rc $out"

echo
if (( failures )); then echo "test_check_actions_pinned_by_sha: $failures assertion(s) FAILED"; exit 1; fi
echo "test_check_actions_pinned_by_sha: OK — all assertions passed."
