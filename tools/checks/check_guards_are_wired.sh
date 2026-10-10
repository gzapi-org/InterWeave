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
# The check lives in agent-fabric, the control plane checked out beside
# this working copy: tools/fabric/github/guards_wired.py, the same for every
# project, run with InterWeave's rules there
# (projects/interweave/integration/gh/guards.json), which this file names. InterWeave's own copy lived here until then; its
# suite, test_check_guards_are_wired.sh, was the port's oracle. This file prints this
# help itself, and otherwise only locates the check, names this working
# copy as --root (an explicit --root after it wins), and hands over. It
# refuses with exit 2 when agent-fabric is not beside this working copy
# — nothing falls back to a stale copy.
#
# Options:
#   --root <dir>   check this repository instead of the one containing
#                  this script
#   -h, --help     this text
#
# Exit codes:
#   0  every guard is wired and self-tested (or explicitly exempt)
#   1  one or more guards are unreachable or untested
#   2  invocation problem (workflows directory missing; or no agent-fabric)
# <<< help

set -uo pipefail
for a in "$@"; do
    case "$a" in -h|--help) sed -n '/^# >>> help$/,/^# <<< help$/p' "$0" | sed -e '1d' -e '$d' -e 's/^# \{0,1\}//'; exit 0 ;; esac
done
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tools/gh/fabric-root.sh
. "$here/../gh/fabric-root.sh"
fabric="$(interweave_fabric_root "$here")"
target="$fabric/tools/fabric/github/guards_wired.py"
rules="$fabric/projects/interweave/integration/gh/guards.json"
[[ -f "$target" && -f "$rules" ]] || {
    echo "check_guards_are_wired: agent-fabric not found at $fabric (expected beside this working copy, as projects/agent-fabric, carrying tools/fabric/github/guards_wired.py and InterWeave's guards.json); set AGENT_FABRIC_ROOT or update it. See CLAUDE.md §9, agent-fabric beside the checkout." >&2
    exit 2
}
# The rules are named rather than found from the working copy's remote: a
# test's sandbox has none the fabric knows. An explicit AGENT_FABRIC_GUARDS_CONFIG wins.
export AGENT_FABRIC_GUARDS_CONFIG="${AGENT_FABRIC_GUARDS_CONFIG:-$rules}"
py="$(interweave_fabric_python check_guards_are_wired)" || exit 127
exec "$py" "$target" --root "$(cd -- "$here/../.." && pwd)" "$@"
