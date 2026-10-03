#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/gh/pr-gate.sh
#
# >>> help
# pr-gate lives in agent-fabric, the control plane checked out beside
# this working copy: runtime/github/pr-gate.sh. It answers "my open
# PRs, how many commits, what stands between each and main" with the
# count rule applied (work commits, review fixes and merges counted
# apart), and lists every branch in flight (--in-flight, --overlap).
# arm.sh reads its --json rows for the count.
#
#   tools/gh/pr-gate.sh              # every open PR of this session
#   tools/gh/pr-gate.sh 161 172      # these PRs, whoever opened them
#   tools/gh/pr-gate.sh --all        # every open PR in the repository
#   tools/gh/pr-gate.sh --in-flight [--path <prefix>]...
#
# This file only locates that script and hands it the arguments and
# stdin untouched. It refuses, loudly, when agent-fabric is not beside
# this working copy — nothing falls back to a stale copy.
#
#   tools/gh/pr-gate.sh --help      # the real script's help
# <<< help
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(git -C "$here" rev-parse --show-toplevel 2>/dev/null || { cd "$here/../.." && pwd; })"
fabric="${AGENT_FABRIC_ROOT:-$root/../agent-fabric}"
target="$fabric/runtime/github/pr-gate.sh"
[[ -f "$target" ]] || {
    echo "pr-gate: agent-fabric not found at $fabric (expected beside this working copy, as projects/agent-fabric); set AGENT_FABRIC_ROOT or check it out. See CLAUDE.md §9, agent-fabric beside the checkout." >&2
    exit 2
}
exec bash "$target" "$@"
