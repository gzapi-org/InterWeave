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
# Checks, for every guard under tools/checks/ and tools/gh/:
#   1. a self-test exists beside it — tools/<dir>/test_<name>.<ext> —
#      unless the guard is listed in tools/checks/selftest_exempt.txt;
#   2. every self-test is itself named in some .github/workflows/*.yml,
#      directly or through a glob the workflow expands. An unwired
#      self-test is the same defect one level up: the suite passes
#      locally and gates nothing;
#   3. every tools/checks/ guard is named in some workflow, for the same
#      reason — the tree checks are the ones that fail a PR.
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
# appears anywhere in it — including inside a `for t in tools/*/test_*.sh`
# loop, which is how the suites are invoked. Matching the basename rather
# than an exact command keeps this from dictating HOW a workflow runs a
# guard, which is not its business.
WORKFLOWS="$(cat "$WORKFLOW_DIR"/*.yml "$WORKFLOW_DIR"/*.yaml 2>/dev/null)"

EXEMPT_FILE="$REPO_ROOT/tools/checks/selftest_exempt.txt"
is_exempt() {
    [ -f "$EXEMPT_FILE" ] || return 1
    grep -qxF "$1" <(sed -e 's/#.*//' -e 's/[[:space:]]//g' "$EXEMPT_FILE" | grep -v '^$')
}

# A glob that the workflow expands counts as naming everything it covers.
wired() {
    local base="$1" dir="$2"
    case "$WORKFLOWS" in
        *"$base"*) return 0 ;;
    esac
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

for dir in checks gh; do
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

        # ...and, in tools/checks, must itself be run by a workflow. The
        # tools/gh scripts are interactive PR helpers a person invokes;
        # their self-tests are what CI runs.
        if [ "$dir" = "checks" ] && ! is_exempt "$rel"; then
            wired "$base" "tools/$dir" \
                || report "$rel: no workflow runs it — it cannot fail a pull request"
        fi
    done
done

if [ "$problems" -gt 0 ]; then
    printf '\ncheck_guards_are_wired: %d unreachable or untested guard(s).\n' "$problems" >&2
    exit 1
fi

printf 'check_guards_are_wired: OK — every guard is wired to a workflow and self-tested.\n'
exit 0
