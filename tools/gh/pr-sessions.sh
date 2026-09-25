#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/gh/pr-sessions.sh
#
# >>> help
# pr-sessions lives in agent-fabric, the control plane checked out beside this
# working copy: runtime/github/pr-sessions.sh. It lists which SESSION owns
# which PR, and what each still owes.
#
# InterWeave kept its own copy until 2026-09-25. It named this session
# <host>/<clone directory>, while this repository's branches carry the
# LOGIN (<host>/<login>/..., bin/fabric-whoami) — so it refused the
# opener's own PR as "another session's" (architect-cto-01 on #114). The
# fabric's copy knows both prefixes. The call goes straight to
# runtime/github/: InterWeave injects no settings (fabric-coordinator).
#
# This file only locates that script and hands it the arguments and
# stdin untouched. It refuses, loudly, when agent-fabric is not beside
# this working copy — nothing falls back to a stale copy.
#
#   tools/gh/pr-sessions.sh --help      # the real script's help
# <<< help
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(git -C "$here" rev-parse --show-toplevel 2>/dev/null || { cd "$here/../.." && pwd; })"
fabric="${AGENT_FABRIC_ROOT:-$root/../agent-fabric}"
target="$fabric/runtime/github/pr-sessions.sh"
[[ -f "$target" ]] || {
    echo "pr-sessions: agent-fabric not found at $fabric (expected beside this working copy, as projects/agent-fabric); set AGENT_FABRIC_ROOT or check it out. See CLAUDE.md §9, agent-fabric beside the checkout." >&2
    exit 2
}
exec bash "$target" "$@"
