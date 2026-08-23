#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# The workflow's job names and CLAUDE.md's list of required contexts
# must be the same set.
#
# WHY THIS EXISTS
#
# A job's `name:` IS its required-check context. CLAUDE.md says so, and
# then said the workflow reports two contexts when it reports three —
# `rust` runs fmt, clippy, and the whole workspace test suite, and the
# document that tells a reader what gates `main` did not mention it.
#
# The direction of the drift is what makes this worth a guard. Nobody
# notices a document that under-claims: the queue keeps working, the
# checks keep running, and the paragraph quietly becomes a description
# of an older repository. The failure only shows up when someone
# consults it to decide whether a rename is safe — and a rename that
# un-gates `main` reports nothing at all, because the ruleset goes on
# requiring a context nothing produces and the queue waits forever.
#
# WHAT THIS CANNOT CHECK
#
# The ruleset itself. Reading `required_status_checks` needs admin API
# access and a network call, which is not something a tree check may
# depend on. So this compares the two things that ARE in the tree, and
# the third leg — that the ruleset requires exactly these — stays a
# manual check. That is a real limit and it is stated rather than
# papered over: agreement here does not prove `main` is gated.
#
# Exit codes:
#   0  the workflow's job names and CLAUDE.md's list agree
#   1  they do not
#   2  invocation error

set -uo pipefail

usage() {
    cat <<'USAGE'
check_required_contexts.sh — CI job names must match CLAUDE.md's list

Usage:
  bash tools/checks/check_required_contexts.sh [--root DIR]

Options:
  --root DIR   repository root (default: the git toplevel)
  --help       this text
USAGE
}

ROOT=""
while [ $# -gt 0 ]; do
    case "$1" in
        --root) ROOT="${2:-}"; shift 2 ;;
        --help|-h) usage; exit 0 ;;
        *) echo "check_required_contexts: unknown option '$1'" >&2; exit 2 ;;
    esac
done

if [ -z "$ROOT" ]; then
    ROOT="$(git rev-parse --show-toplevel 2>/dev/null || true)"
fi
[ -n "$ROOT" ] || { echo "check_required_contexts: no --root and not in a git repo" >&2; exit 2; }
[ -d "$ROOT" ] || { echo "check_required_contexts: not a directory: $ROOT" >&2; exit 2; }

WORKFLOW="$ROOT/.github/workflows/ci.yml"
CONTRACT="$ROOT/CLAUDE.md"
[ -f "$WORKFLOW" ] || { echo "check_required_contexts: missing $WORKFLOW" >&2; exit 2; }
[ -f "$CONTRACT" ] || { echo "check_required_contexts: missing $CONTRACT" >&2; exit 2; }

# A job `name:` is indented exactly four spaces under `jobs:`; a step
# `name:` is deeper and prefixed with `- `. Anchoring on that is what
# separates the three contexts from the twenty step labels.
jobs="$(sed -n 's/^    name: *\(.*\)$/\1/p' "$WORKFLOW" | sed 's/^"\(.*\)"$/\1/' | sort -u)"

# The sentence in §9 names them in bold backticks. Read from there
# rather than from a list kept somewhere else, because the paragraph a
# reader actually consults is the thing that has to be true.
line="$(grep -n 'It reports .* contexts, which are the job' "$CONTRACT" | head -1)"
if [ -z "$line" ]; then
    echo "check_required_contexts: CLAUDE.md no longer states which contexts CI reports" >&2
    exit 1
fi
claimed="$(printf '%s' "$line" | grep -o '\*\*`[^`]*`\*\*' | tr -d '*`' | sort -u)"

if [ -z "$claimed" ]; then
    echo "check_required_contexts: CLAUDE.md names no contexts in that sentence" >&2
    exit 1
fi

if [ "$jobs" != "$claimed" ]; then
    {
        echo "check_required_contexts: the workflow and CLAUDE.md disagree."
        echo
        echo "  ci.yml job names:"
        printf '    %s\n' $jobs
        echo "  CLAUDE.md says CI reports:"
        printf '    %s\n' $claimed
        echo
        echo "A job's name IS its required-check context. Renaming a job without"
        echo "updating the ruleset leaves main gated on a context nothing reports,"
        echo "and the merge queue waits forever."
    } >&2
    exit 1
fi

count="$(printf '%s\n' "$jobs" | wc -l | tr -d ' ')"
printf 'check_required_contexts: OK — %d job name(s) match CLAUDE.md; the ruleset itself is a manual check.\n' "$count"
