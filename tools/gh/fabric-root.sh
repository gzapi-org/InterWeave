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

interweave_fabric_python() {  # interweave_fabric_python <caller's name, for the message>
    # The fleet's pinned Python (agent-fabric runtime/python.json, one per
    # host), which the fabric's tools/fabric/github modules run on; the
    # hand-overs call agent-fabric's commands (bin/fabric-pr <verb>, or a
    # module here), never its runtime/github/*.sh shims, which are being
    # removed. AGENT_FABRIC_PYTHON points elsewhere for a test or a host
    # without it. Missing: the shim's own message and its exit, 127.
    local py="${AGENT_FABRIC_PYTHON:-/usr/local/bin/fabric-python}"
    if [[ ! -x "$py" ]]; then
        echo "$1: the fleet's pinned Python is not installed at $py; as root: /usr/bin/python3 <agent-fabric>/tools/fabric/python_pin.py install" >&2
        return 127
    fi
    printf '%s' "$py"
}
