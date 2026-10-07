#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/gh/actions-health.sh
#
# >>> help
# Is it worth spending CI minutes right now? GitHub Actions' status and
# the organisation's included allowance, in one line and one exit code
# (0 healthy, 1 degraded, 2 could not find out).
#
# actions-health lives in agent-fabric, the control plane checked out
# beside this working copy: runtime/github/actions-health.sh. This file
# only locates it and hands it the arguments untouched, so every
# documented invocation (tools/gh/actions-health.sh, --quiet, --json,
# --included N, --org NAME) keeps working.
#
# InterWeave's allowance setting is INTERWEAVE_ACTIONS_INCLUDED_MINUTES.
# It is passed on as the fabric's AGENT_FABRIC_ACTIONS_INCLUDED_MINUTES
# (an explicit value of that wins), and the fabric's lines name
# INTERWEAVE_ACTIONS_INCLUDED_MINUTES as the setting to change. This
# repository is public: its own runs on standard runners bill nothing,
# which the fabric's check reports as such.
#
# It refuses with exit 2 when agent-fabric is not beside this working
# copy, and maps the fabric wrapper's 127 (its pinned Python missing) to
# 2: this script answers 0, 1 or 2.
#
#   tools/gh/actions-health.sh --help      # the real script's help
# <<< help
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(git -C "$here" rev-parse --show-toplevel 2>/dev/null || { cd "$here/../.." && pwd; })"
fabric="${AGENT_FABRIC_ROOT:-$root/../agent-fabric}"
target="$fabric/runtime/github/actions-health.sh"
[[ -f "$target" ]] || {
    echo "actions-health: agent-fabric not found at $fabric (expected beside this working copy, as projects/agent-fabric, carrying runtime/github/actions-health.sh); set AGENT_FABRIC_ROOT or update it. See CLAUDE.md §9, agent-fabric beside the checkout." >&2
    exit 2
}
if [[ -z "${AGENT_FABRIC_ACTIONS_INCLUDED_MINUTES:-}" && -n "${INTERWEAVE_ACTIONS_INCLUDED_MINUTES:-}" ]]; then
    export AGENT_FABRIC_ACTIONS_INCLUDED_MINUTES="$INTERWEAVE_ACTIONS_INCLUDED_MINUTES"
fi
export AGENT_FABRIC_ACTIONS_INCLUDED_SETTING=INTERWEAVE_ACTIONS_INCLUDED_MINUTES
rc=0; bash "$target" "$@" || rc=$?
[[ $rc -eq 127 ]] && exit 2
exit "$rc"
