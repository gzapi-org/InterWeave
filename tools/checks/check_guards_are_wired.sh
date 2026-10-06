#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/check_guards_are_wired.sh
#
# >>> help
# Prove every guard in tools/ is REACHABLE — invoked by a workflow, and
# paired with a self-test.
#
#   tools/checks/check_guards_are_wired.sh
#   tools/checks/check_guards_are_wired.sh --root <dir>
#
# Why this exists: a guard that runs nowhere passes silently-green
# forever. Every check in this repository was written, tested by hand,
# committed — and invoked by nothing at all until CI landed. Running a
# script by hand proves the script works; it says nothing about whether
# anything will ever call it again. Verifying the artifact is not
# verifying its reachability.
#
# Checks, for every guard under tools/checks/, tools/gh/, tools/ci/ and
# tools/host/android/:
#   1. a self-test exists beside it — tools/<dir>/test_<name>.<ext> —
#      unless the guard is listed in tools/checks/selftest_exempt.txt;
#   2. every self-test is itself named in some .github/workflows/*.yml,
#      directly or through a glob the workflow expands. An unwired
#      self-test is the same defect one level up: the suite passes
#      locally and gates nothing;
#   3. every tools/checks/ and tools/ci/ script is named in some workflow,
#      for the same reason — the tree checks are the ones that fail a PR,
#      and a tools/ci/ script exists only to be run by one (a session
#      wrapper a job runs its tests under).
#
# NOT checked here: whether the paths a guard protects actually trigger
# the job that runs it. "Runs at all" and "runs on the right changes" are
# different failures with different fixes; this file only answers the
# first, and answers it completely.
#
# Options:
#   --root <dir>   check this repository instead of the one containing
#                  this script
#   -h, --help     this text
#
# Exit codes:
#   0  every guard is wired and self-tested (or explicitly exempt)
#   1  one or more guards are unreachable or untested
#   2  invocation problem (workflows directory missing)
# <<< help

set -uo pipefail

SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
REPO_ROOT="$( cd -- "$SCRIPT_DIR/../.." && pwd )"

show_help() {
    sed -n '/^# >>> help$/,/^# <<< help$/p' "$0" | sed -e '1d' -e '$d' -e 's/^# \{0,1\}//'
}

while [ $# -gt 0 ]; do
    case "$1" in
        -h|--help) show_help; exit 0 ;;
        --root)    [ $# -ge 2 ] || { echo "--root needs a value" >&2; exit 2; }
                   REPO_ROOT="$2"; shift 2 ;;
        *)         echo "check_guards_are_wired: unexpected argument: $1" >&2; exit 2 ;;
    esac
done

WORKFLOW_DIR="$REPO_ROOT/.github/workflows"
if [ ! -d "$WORKFLOW_DIR" ]; then
    echo "check_guards_are_wired: no .github/workflows in $REPO_ROOT" >&2
    exit 2
fi

# One blob of every workflow. A guard counts as wired if its basename
# appears in it as a whole name, not inside a longer file name (wired()),
# or, for a self-test, if a `for t in tools/<dir>/test_*.sh` loop covers it,
# which is how the suites are invoked. Matching the basename rather
# than an exact command keeps this from dictating HOW a workflow runs a
# guard, which is not its business — with one exception, below: a loop
# over self-tests must run them through tools/checks/run_suite.sh.
#
# WHOLE-LINE COMMENTS ARE DROPPED FIRST. A comment that names a guard --
# a YAML note explaining a step, or a commented-out command inside a
# `run: |` block -- runs nothing, and it counted: a guard whose step was
# deleted stayed "wired" through a sentence elsewhere in the file that
# mentioned it (measured 2026-09-29, check_actions_pinned_by_sha.sh). A
# comment trailing a line is left in: `#` also opens `${#arr}` and sits
# inside quoted strings, and cutting there would drop real invocations.
WORKFLOWS="$(cat "$WORKFLOW_DIR"/*.yml "$WORKFLOW_DIR"/*.yaml 2>/dev/null | sed -e '/^[[:space:]]*#/d')"

EXEMPT_FILE="$REPO_ROOT/tools/checks/selftest_exempt.txt"
is_exempt() {
    [ -f "$EXEMPT_FILE" ] || return 1
    grep -qxF "$1" <(sed -e 's/#.*//' -e 's/[[:space:]]//g' "$EXEMPT_FILE" | grep -v '^$')
}

# A glob that the workflow expands counts as naming everything it covers.
wired() {
    local base="$1" dir="$2"
    # The basename as a whole name, not a substring: `check_x.sh` is not
    # wired by a workflow that names only `test_check_x.sh`, which runs the
    # self-test and never the guard. A name character on either side means
    # a different, longer name.
    if grep -qE "(^|[^A-Za-z0-9_.-])${base//./\\.}([^A-Za-z0-9_.-]|\$)" <<<"$WORKFLOWS"; then
        return 0
    fi
    # `tools/gh/test_*.sh` covers tools/gh/test_anything.sh
    case "$base" in
        test_*)
            case "$WORKFLOWS" in
                *"$dir/test_*"*) return 0 ;;
            esac ;;
    esac
    return 1
}

problems=0
report() { printf '%s\n' "$1"; problems=$((problems + 1)); }

for dir in checks gh ci host/android; do
    d="$REPO_ROOT/tools/$dir"
    [ -d "$d" ] || continue

    for path in "$d"/*; do
        [ -f "$path" ] || continue
        base="$(basename "$path")"
        case "$base" in
            *.sh|*.py) ;;
            *) continue ;;
        esac

        rel="tools/$dir/$base"
        stem="${base%.*}"

        if [ "${base#test_}" != "$base" ]; then
            # A SELF-TEST. It must be invoked by a workflow.
            wired "$base" "tools/$dir" \
                || report "$rel: no workflow runs it — the suite passes locally and gates nothing"
            continue
        fi

        # A GUARD. It needs a self-test unless exempt...
        if ! is_exempt "$rel"; then
            if ! ls "$d/test_$stem".* >/dev/null 2>&1; then
                report "$rel: no self-test beside it (add tools/$dir/test_$stem.sh, or exempt it)"
            fi
        fi

        # ...and, in tools/checks and tools/ci, must itself be run by a
        # workflow. The tools/gh scripts are interactive PR helpers, and
        # tools/host/ ones host provisioning, that a person invokes; their
        # self-tests are what CI runs.
        if [ "$dir" != "gh" ] && [ "${dir%%/*}" != "host" ] && ! is_exempt "$rel"; then
            wired "$base" "tools/$dir" \
                || report "$rel: no workflow runs it — it cannot fail a pull request"
        fi
    done
done

# EVERY LOOP THAT RUNS SELF-TESTS RUNS THEM THROUGH run_suite.sh. Run
# bare, a suite whose assertion calls an undefined helper prints "command
# not found" and passes; tools/checks/run_suite.sh is what fails it. The
# glob counts as wiring above whatever the loop body does, so this is the
# one place HOW a workflow runs something is this script's business. Each
# `for <var> in … tools/<dir>/test_*.sh …` is read up to its `done`.
while IFS= read -r loop; do
    [ -n "$loop" ] && report "a workflow loop over self-tests runs them without tools/checks/run_suite.sh: $loop"
done < <(awk '
    !inloop && /for [A-Za-z_][A-Za-z0-9_]* in .*tools\/[a-z]+\/test_\*\.sh/ {
        inloop = 1; head = $0; body = ""
        sub(/^[[:space:]]+/, "", head)
    }
    inloop {
        body = body "\n" $0
        if ($0 ~ /(^|[;[:space:]])done([;[:space:]]|$)/) {
            if (body !~ /run_suite\.sh/) print head
            inloop = 0
        }
    }
    END { if (inloop && body !~ /run_suite\.sh/) print head }
' <<<"$WORKFLOWS")

if [ "$problems" -gt 0 ]; then
    printf '\ncheck_guards_are_wired: %d unreachable or untested guard(s), or a bare self-test loop.\n' "$problems" >&2
    exit 1
fi

printf 'check_guards_are_wired: OK — every guard is wired to a workflow and self-tested.\n'
exit 0
