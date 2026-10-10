#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/run_suite.sh
#
# Runs a shell self-test so that an UNDEFINED COMMAND fails it — the
# drop-in for `bash <suite>` that CI's self-test loop and `cargo xtask
# selftests` run every test_*.sh through:
#
#   bash tools/checks/run_suite.sh tools/checks/test_check_spike_locks.sh
#
# Why it exists: a suite counts failures in a variable, and a call to a
# helper that was never defined (a mistyped `assert_contians`) makes bash
# print "command not found", return 127 and carry on — the counter never
# moves and the suite announces "all assertions passed" with that check
# skipped. This repository shipped two such assertions (5f2c0c9). The
# runner exports a command_not_found_handle that records every such call
# and fails the run.
#
# The runner lives in agent-fabric, the control plane checked out beside
# this working copy: tools/fabric/github/run_suite.py, the same for every
# project (its help and tools/fabric/github/run_suite.py carry the full
# reasoning, including why a static scan of the suites was abandoned).
# InterWeave's own copy lived here until then; test_run_suite.sh runs
# unchanged against this hand-over: it was the port's oracle.
#
# This file only locates the runner and hands it the arguments, stdin and
# environment untouched. It refuses with exit 2 (usage, never the
# suite's 1) when agent-fabric is not beside this working copy — nothing
# falls back to a stale copy.
#
# Exit codes:
#   0  the suite passed and every command it ran resolved
#   1  a command did not resolve (named on stderr), or the suite failed
#   2  usage, or agent-fabric not found

set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tools/gh/fabric-root.sh
. "$here/../gh/fabric-root.sh"
fabric="$(interweave_fabric_root "$here")"
target="$fabric/tools/fabric/github/run_suite.py"
[[ -f "$target" ]] || {
    echo "run_suite: agent-fabric not found at $fabric (expected beside this working copy, as projects/agent-fabric, carrying tools/fabric/github/run_suite.py); set AGENT_FABRIC_ROOT or update it. See CLAUDE.md §9, agent-fabric beside the checkout." >&2
    exit 2
}
py="$(interweave_fabric_python run_suite)" || exit 127
exec "$py" "$target" "$@"
