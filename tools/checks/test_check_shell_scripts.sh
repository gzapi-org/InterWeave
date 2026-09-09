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
# most of the tree to satisfy a note about a construct it uses on
# purpose.
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
#
# NOT RUN LOCALLY WHEN WRITTEN: the machine these cases were authored on
# had shellcheck installed but not yet on PATH, so the first execution of
# this file was CI's, against the pinned 0.11.0. Said here because a
# self-test that was never watched failing is one more claim.

set -uo pipefail

SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
UNDER_TEST="$SCRIPT_DIR/check_shell_scripts.sh"
[[ -f "$UNDER_TEST" ]] || { echo "test: $UNDER_TEST not found" >&2; exit 1; }

if ! command -v shellcheck >/dev/null 2>&1; then
    echo "test_check_shell_scripts: shellcheck is not installed, so the guard cannot be exercised." >&2
    echo "  Install it (see check_shell_scripts.sh --help) and run this again." >&2
    exit 2
fi

# PINNED, NOT INHERITED. The guard reads
# `INTERWEAVE_SHELLCHECK_SEVERITY` from the environment, so a suite that
# let it through would pass or fail on ambient state -- an exported
# `error` makes the two finding cases report clean. The hatch gets its
# own case below instead. Review finding on PR #82.
unset INTERWEAVE_SHELLCHECK_SEVERITY
# And shellcheck's own hatch, for the same reason -- the guard unsets it
# too, and case 8 below is what proves that rather than this line.
unset SHELLCHECK_OPTS

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

# THE EXIT CODE, not merely non-zero.
#
# Exit 2 means "looked at nothing" and exit 1 means "found something", and
# an assertion that only reads non-zero treats them as the same answer --
# so the two positive cases below would pass whenever the sandbox's `git
# add` silently produced nothing and shellcheck was never invoked at all.
# That is the shape this file's header says it exists to prevent.
# Review finding on PR #82.
#
# THE OUTPUT IS KEPT, and shown on failure. `>/dev/null 2>&1` left a
# failing case reporting `(expected exit 1, got 2)` and nothing about
# why, which from a CI log is a number with no cause. Review finding on
# PR #82.
#
# AND EVERY CASE READS AN EXACT CODE, including the ones that set an
# environment variable or pass an argument -- which is what the
# `VAR=value` and `-- argument` forms below are for. Three cases used to
# read only non-zero, and when a broken file-list read made the guard
# exit 2 for everything, all three passed while five others failed:
# exactly the collapse the paragraph above describes. Review finding on
# PR #82.
#
# usage: guard_run [VAR=value ...] [-- guard-argument ...]
guard_run() {
    local -a envs=()
    while [[ $# -gt 0 && "$1" != "--" ]]; do envs+=("$1"); shift; done
    [[ "${1:-}" == "--" ]] && shift
    GUARD_OUTPUT=$( cd "$SANDBOX" && env "${envs[@]}" bash tools/checks/check_shell_scripts.sh "$@" 2>&1 )
    GUARD_STATUS=$?
}

expect_status() {
    local want="$1" label="$2"
    shift 2
    guard_run "$@"
    if [[ "$GUARD_STATUS" == "$want" ]]; then
        pass "$label"
    else
        fail "$label (expected exit $want, got $GUARD_STATUS)"
        printf '%s\n' "$GUARD_OUTPUT" | sed 's/^/      | /' >&2
    fi
}

echo "test_check_shell_scripts: exercising the guard"

# 0. THE BASELINE, and the count. Every exit-0 case below also depends
#    on the guard's own copy being clean -- the sandbox tracks it -- so
#    one warning in the guard would fail three cases with messages
#    pointing at fixtures. This case names that dependency. And it reads
#    the COUNT the OK line prints: the file-list read once handed the
#    linter every path concatenated into one, which no case noticed
#    because none read how many scripts were judged. Review findings on
#    PR #82.
sandbox_with '#!/usr/bin/env bash
echo fine'
expect_status 0 "the guard's own copy plus one clean script is exit 0"
if [[ "$GUARD_OUTPUT" == *"OK — 2 tracked shell scripts"* ]]; then
    pass "and the OK line counts exactly the two scripts the sandbox tracks"
else
    fail "the OK line must count 2 scripts (the fixture and the guard's copy), got: $GUARD_OUTPUT"
fi
cleanup; SANDBOX=""

# 1. THE POSITIVE CASE. SC2155: `export X="$(...)"` masks the
#    substitution's status, which is the class the guard found in the real
#    tree when it was written.
sandbox_with '#!/usr/bin/env bash
export THING="$(echo value)"
echo "$THING"'
expect_status 1 "a masked exit status (SC2155) is refused as a FINDING"
cleanup; SANDBOX=""

# 2. SC2034: an assignment nothing reads. The second warning class the
#    guard found in the real tree.
sandbox_with '#!/usr/bin/env bash
unused_value=1
echo done'
expect_status 1 "an unused assignment (SC2034) is refused as a FINDING"
cleanup; SANDBOX=""

# 3. THE NEGATIVE CONTROL, and the reason the threshold is `warning`:
#    the `A && B || { fail; }` idiom is SC2015 at INFO and is used
#    deliberately throughout this repository. It must pass.
sandbox_with '#!/usr/bin/env bash
value=5
[ "$value" -ge 1 ] && [ "$value" -le 10 ] \
  || { echo "out of range" >&2; exit 2; }
echo "$value"'
expect_status 0 "the deliberate A && B || { fail; } idiom passes at warning severity"
#    AND THE SAME FIXTURE IS REFUSED AT `info` -- the half that makes
#    this a test of the threshold rather than of an empty finding set.
#    Exit 0 above is equally true if SC2015 never fires at all: a
#    linter release that stops flagging this shape, or a "simplified"
#    fixture, would leave the case passing forever while asserting
#    nothing. Exit 1 here proves the finding exists and that `warning`
#    is what admits it. Review finding on PR #82.
expect_status 1 "and is refused at severity=info, so SC2015 fires and warning is the threshold that admits it" \
    INTERWEAVE_SHELLCHECK_SEVERITY=info
cleanup; SANDBOX=""

# 4. A TARGETED DISABLE IS HONOURED, because the guard's own message
#    tells an author to use one and a guard that ignored them would make
#    that advice a lie.
sandbox_with '#!/usr/bin/env bash
# shellcheck disable=SC2155 # deliberate, for this test
export THING="$(echo value)"
echo "$THING"'
expect_status 0 "a targeted disable with a reason is honoured"
cleanup; SANDBOX=""

# 5. AN EMPTY FILE SET IS NOT A PASS. Without this the guard would report
#    OK on a tree where `git ls-files` matched nothing — the
#    passing-by-looking-at-nothing shape.
SANDBOX="$(mktemp -d)"
mkdir -p "$SANDBOX/tools/checks"
cp "$UNDER_TEST" "$SANDBOX/tools/checks/"
git -C "$SANDBOX" init --quiet
# The guard itself is untracked here, so `git ls-files '*.sh'` is empty.
expect_status 2 "an empty file set is refused as LOOKED AT NOTHING, not as a finding"
cleanup; SANDBOX=""

# 6. THE SEVERITY HATCH RELAXES THE GUARD, and that is worth a case
#    rather than a sentence: `--severity` is a MINIMUM, so `error` reports
#    LESS than `warning`. A reader who thought raising it tightened the
#    gate would have it backwards, and the guard's failure message says so
#    in words that nothing checked. Review finding on PR #82.
sandbox_with '#!/usr/bin/env bash
export THING="$(echo value)"
echo "$THING"'
expect_status 0 "severity=error hides a warning-level finding, so the hatch relaxes rather than tightens" \
    INTERWEAVE_SHELLCHECK_SEVERITY=error
cleanup; SANDBOX=""

# 7. A TRACKED FILE THE WORKTREE DOES NOT HAVE IS "LOOKED AT LESS THAN
#    EVERYTHING", exit 2 -- not a finding. shellcheck exits 2 when it
#    cannot open a file, and the first guard collapsed that into its own
#    exit 1 with advice to add a targeted disable for a file that does
#    not exist. Mid-rebase and `git rm --cached` both produce this state.
#    Review finding on PR #82.
sandbox_with '#!/usr/bin/env bash
echo fine'
rm "$SANDBOX/scripts/case.sh"
expect_status 2 "a tracked file missing from the worktree is refused as COULD NOT JUDGE, not as a finding"
cleanup; SANDBOX=""

# 8. SHELLCHECK'S OWN HATCH IS NEUTRALISED. `SHELLCHECK_OPTS='-e SC2155'`
#    in a developer's environment would otherwise suppress the class and
#    print an OK line naming a threshold that was not applied. The guard
#    unsets it, and this is the case that would fail if it stopped.
#    Review finding on PR #82.
sandbox_with '#!/usr/bin/env bash
export THING="$(echo value)"
echo "$THING"'
expect_status 1 "an inherited SHELLCHECK_OPTS exclusion does not reach shellcheck" \
    SHELLCHECK_OPTS='-e SC2155'
cleanup; SANDBOX=""

# 9. AN UNKNOWN ARGUMENT IS REFUSED, exit 2, rather than ignored. The
#    guard's subject is a file list, so a path argument silently judging
#    every file while the caller believes one was singled out is the
#    worst reading available. Review finding on PR #82.
sandbox_with '#!/usr/bin/env bash
echo fine'
expect_status 2 "an unexpected argument is refused rather than ignored" -- scripts/case.sh
cleanup; SANDBOX=""

if [[ $failures -gt 0 ]]; then
    echo "test_check_shell_scripts: FAILED — $failures assertion(s)" >&2
    exit 1
fi
echo "test_check_shell_scripts: OK — all assertions passed."
