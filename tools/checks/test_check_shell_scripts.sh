#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/test_check_shell_scripts.sh
#
# Self-test for check_shell_scripts.sh.
#
# THE CASE THAT MATTERS IS THE POSITIVE ONE: a script with a real
# warning-level defect must be REFUSED. A linting guard that cannot fail
# reports OK forever over a tree nobody has checked, and reads as
# coverage — which is worse than not having it, because the gap stops
# being visible.
#
# The negative cases exist so the guard is not refused for crying wolf:
# the idiom this repository uses deliberately (`A && B || { fail; }`,
# SC2015 at info) must NOT fail it, or the guard would demand rewriting
# 39 scripts to satisfy a note about a construct they use on purpose.
#
# Each case runs the guard against a SANDBOX git repository rather than
# the real tree, because the guard takes its file list from `git ls-files`
# — so a test that edited the real tree would be testing the repository's
# current state rather than the guard.
#
# Exit codes:
#   0  all assertions passed
#   1  one or more failed
#   2  shellcheck is not installed, so none of this can be exercised

set -uo pipefail

SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
UNDER_TEST="$SCRIPT_DIR/check_shell_scripts.sh"
[[ -f "$UNDER_TEST" ]] || { echo "test: $UNDER_TEST not found" >&2; exit 1; }

if ! command -v shellcheck >/dev/null 2>&1; then
    echo "test_check_shell_scripts: shellcheck is not installed, so the guard cannot be exercised." >&2
    echo "  Install it (see check_shell_scripts.sh --help) and run this again." >&2
    exit 2
fi

failures=0
SANDBOX=""
cleanup() { [[ -n "$SANDBOX" && -d "$SANDBOX" ]] && rm -rf "$SANDBOX"; }
trap cleanup EXIT

pass() { echo "  ✓ $1"; }
fail() { echo "  ✗ $1" >&2; failures=$((failures + 1)); }

# A sandbox repository holding `scripts/case.sh` with the given body, plus
# a copy of the guard at the path the guard expects relative to itself.
sandbox_with() {
    local body="$1"
    SANDBOX="$(mktemp -d)"
    mkdir -p "$SANDBOX/tools/checks" "$SANDBOX/scripts"
    cp "$UNDER_TEST" "$SANDBOX/tools/checks/"
    printf '%s\n' "$body" > "$SANDBOX/scripts/case.sh"
    git -C "$SANDBOX" init --quiet
    git -C "$SANDBOX" add -A
}

run_guard() {
    ( cd "$SANDBOX" && bash tools/checks/check_shell_scripts.sh >/dev/null 2>&1 )
}

echo "test_check_shell_scripts: exercising the guard"

# 1. THE POSITIVE CASE. SC2155: `export X="$(...)"` masks the
#    substitution's status, which is the class the guard found in the real
#    tree when it was written.
sandbox_with '#!/usr/bin/env bash
export THING="$(echo value)"
echo "$THING"'
if run_guard; then
    fail "a masked exit status (SC2155) must be refused"
else
    pass "a masked exit status (SC2155) is refused"
fi
cleanup; SANDBOX=""

# 2. SC2034: an assignment nothing reads. The second warning class the
#    guard found in the real tree.
sandbox_with '#!/usr/bin/env bash
unused_value=1
echo done'
if run_guard; then
    fail "an unused assignment (SC2034) must be refused"
else
    pass "an unused assignment (SC2034) is refused"
fi
cleanup; SANDBOX=""

# 3. THE NEGATIVE CONTROL, and the reason the threshold is `warning`:
#    the `A && B || { fail; }` idiom is SC2015 at INFO and is used
#    deliberately throughout this repository. It must pass.
sandbox_with '#!/usr/bin/env bash
value=5
[ "$value" -ge 1 ] && [ "$value" -le 10 ] \
  || { echo "out of range" >&2; exit 2; }
echo "$value"'
if run_guard; then
    pass "the deliberate A && B || { fail; } idiom passes at warning severity"
else
    fail "SC2015 is info-level and must not fail the guard"
fi
cleanup; SANDBOX=""

# 4. A TARGETED DISABLE IS HONOURED, because the guard's own message
#    tells an author to use one and a guard that ignored them would make
#    that advice a lie.
sandbox_with '#!/usr/bin/env bash
# shellcheck disable=SC2155 # deliberate, for this test
export THING="$(echo value)"
echo "$THING"'
if run_guard; then
    pass "a targeted disable with a reason is honoured"
else
    fail "a targeted disable must suppress its own finding"
fi
cleanup; SANDBOX=""

# 5. AN EMPTY FILE SET IS NOT A PASS. Without this the guard would report
#    OK on a tree where `git ls-files` matched nothing — the
#    passing-by-looking-at-nothing shape.
SANDBOX="$(mktemp -d)"
mkdir -p "$SANDBOX/tools/checks"
cp "$UNDER_TEST" "$SANDBOX/tools/checks/"
git -C "$SANDBOX" init --quiet
# The guard itself is untracked here, so `git ls-files '*.sh'` is empty.
if run_guard; then
    fail "an empty file set must not pass"
else
    pass "an empty file set is refused rather than passing"
fi
cleanup; SANDBOX=""

if [[ $failures -gt 0 ]]; then
    echo "test_check_shell_scripts: FAILED — $failures assertion(s)" >&2
    exit 1
fi
echo "test_check_shell_scripts: OK — all assertions passed."
