#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/gh/fabric-root.sh
#
# Where agent-fabric, the control plane, is checked out for a script in
# this working copy. Sourced by the tools/gh scripts that hand over to it;
# defines interweave_fabric_root and nothing else.
#
#   . "$here/fabric-root.sh"; fabric="$(interweave_fabric_root "$here")"
#
# AGENT_FABRIC_ROOT wins (the session-start hook sets it). Otherwise it is
# ../agent-fabric beside the CLONE, not beside the working tree: a git
# worktree (a subagent's under .claude/worktrees/, or one added by hand)
# shares the clone's .git, and agent-fabric sits beside the clone. Read
# from the worktree's own top level, every hand-over looked for
# .claude/worktrees/agent-fabric and exited 2. Outside a repository, the
# directory two above the script stands for the top level, as before.

interweave_fabric_root() {  # interweave_fabric_root <a directory inside the working copy>
    if [[ -n "${AGENT_FABRIC_ROOT:-}" ]]; then printf '%s' "$AGENT_FABRIC_ROOT"; return; fi
    local common main
    common="$(git -C "$1" rev-parse --path-format=absolute --git-common-dir 2>/dev/null || true)"
    if [[ "$common" == */.git ]]; then
        main="${common%/.git}"
    else
        main="$(git -C "$1" rev-parse --show-toplevel 2>/dev/null || { cd "$1/../.." && pwd; })"
    fi
    printf '%s' "$main/../agent-fabric"
}
