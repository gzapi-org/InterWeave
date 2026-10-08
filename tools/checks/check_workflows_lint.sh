#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# tools/checks/check_workflows_lint.sh
#
# >>> help
# Does every workflow pass actionlint and zizmor?
#
#   tools/checks/check_workflows_lint.sh
#   tools/checks/check_workflows_lint.sh --root <dir>
#
# Every workflow passes actionlint (its run: scripts through shellcheck)
# and zizmor (the workflow security audit), with the release of each
# pinned HERE, once, for tree checks and for `cargo xtask checks` before a push.
#
# WHY ONE SCRIPT. The two tools used to be downloaded inline in
# the tree-checks job, so a session could learn of a finding only from a
# pushed run, and a local run would have had to copy the pins — two
# copies of a version drift, and the one CI uses is the one that counts.
#
# WHAT EACH CATCHES. actionlint: a run: script that does not parse (a
# broken script fails only on the path that reaches the broken line —
# a sibling repository's weekly report died on the one run it existed for), and shellcheck
# warnings. Info- and style-level notes are ignored: which fire varies with
# the shellcheck release. zizmor, offline: ${{ }} spliced into a script,
# over-broad permissions, persisted checkout tokens, spoofable bot checks,
# inherited secrets. A rule this repository decides against would go in a
# .github/zizmor.yml with its reason (there is none today: every rule is
# on); a single site is excused inline with `# zizmor: ignore[<rule>]`
# and its reason.
#
# THE TOOLS are release binaries, fetched once into the cache
# (INTERWEAVE_TOOL_CACHE, else $XDG_CACHE_HOME/interweave-tools, else
# ~/.cache/interweave-tools; CI points it at $RUNNER_TEMP), each checked
# against its sha256 before it is extracted or run. A cached binary is
# reused only from a directory named for its version and digest, so a
# pin bump fetches afresh. shellcheck comes from PATH: actionlint skips
# its script pass SILENTLY without one, so its absence is exit 2.
#
# The check lives in agent-fabric, the control plane checked out beside
# this working copy: runtime/github/check-workflows-lint.sh, the same for every
# project. InterWeave's own copy lived here until then; its
# suite, test_check_workflows_lint.sh, was the port's oracle. This file prints this
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
#   0  both tools ran and found nothing
#   1  a finding (printed)
#   2  a tool could not be obtained or run (no network, a checksum
#      mismatch, no shellcheck, no agent-fabric) — nothing was checked
#   130, 143  interrupted by INT or TERM; nothing of the fetch is left
# <<< help

set -uo pipefail
for a in "$@"; do
    case "$a" in -h|--help) sed -n '/^# >>> help$/,/^# <<< help$/p' "$0" | sed -e '1d' -e '$d' -e 's/^# \{0,1\}//'; exit 0 ;; esac
done
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tools/gh/fabric-root.sh
. "$here/../gh/fabric-root.sh"
fabric="$(interweave_fabric_root "$here")"
target="$fabric/runtime/github/check-workflows-lint.sh"
[[ -f "$target" ]] || {
    echo "check_workflows_lint: agent-fabric not found at $fabric (expected beside this working copy, as projects/agent-fabric, carrying runtime/github/check-workflows-lint.sh); set AGENT_FABRIC_ROOT or update it. See CLAUDE.md §9, agent-fabric beside the checkout." >&2
    exit 2
}
# InterWeave's name for the cache (CI's step sets it) under the fabric's;
# an explicit AGENT_FABRIC_TOOL_CACHE wins.
[[ -z "${AGENT_FABRIC_TOOL_CACHE:-}" && -n "${INTERWEAVE_TOOL_CACHE:-}" ]] && export AGENT_FABRIC_TOOL_CACHE="$INTERWEAVE_TOOL_CACHE"
exec bash "$target" --root "$(cd -- "$here/../.." && pwd)" "$@"
