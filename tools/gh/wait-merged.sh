#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/gh/wait-merged.sh
#
# >>> help
# Block until a PR is decided, then exit saying which. The point is the
# EXIT: run it in the background and the exit is the callback.
#
#   tools/gh/wait-merged.sh <pr-number> [--interval D] [--timeout D] [-q]
#   tools/gh/wait-merged.sh --help     # the real script's help: every verdict and why
#
# Every run ends with ONE line on stdout naming the outcome and its exit:
#   0  merged                     4  still open at the timeout
#   2  usage, or PR unreadable    5  blocked: a check failed, or a conflict
#   3  closed WITHOUT merging     6  stalled for a cause outside the PR
#
# wait-merged lives in agent-fabric, the control plane checked out beside
# this working copy: runtime/github/wait-merged.sh, the same for every
# project. InterWeave's own copy lived here until then. This file only
# locates it and hands it the arguments untouched, naming InterWeave's
# arm.json (the arm command its lines suggest) and passing
# INTERWEAVE_ACTIONS_INCLUDED_MINUTES on as the fabric's
# AGENT_FABRIC_ACTIONS_INCLUDED_MINUTES, with the setting its lines name
# to a person InterWeave's (an explicit value of the fabric's wins). The
# repository is GH_REPO, else this working copy's origin on github.com:
# GH_REPO_OWNER and GH_REPO_NAME are no longer read.
#
# It refuses with exit 2 when agent-fabric is not beside this working
# copy — nothing falls back to a stale copy. It execs, so a signal that
# stops the watch stops the fabric's script; the price is that a host
# without the fleet's pinned Python exits 127 (the fabric's message says
# how to install it), outside the table above.
# <<< help
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tools/gh/fabric-root.sh
. "$here/fabric-root.sh"
fabric="$(interweave_fabric_root "$here")"
target="$fabric/runtime/github/wait-merged.sh"
rules="$fabric/projects/interweave/integration/gh/arm.json"
[[ -f "$target" && -f "$rules" ]] || {
    echo "wait-merged: agent-fabric not found at $fabric (expected beside this working copy, as projects/agent-fabric, carrying runtime/github/wait-merged.sh and InterWeave's arm.json); set AGENT_FABRIC_ROOT or update it. See CLAUDE.md §9, agent-fabric beside the checkout." >&2
    exit 2
}
export AGENT_FABRIC_ARM_CONFIG="${AGENT_FABRIC_ARM_CONFIG:-$rules}"
[[ -z "${AGENT_FABRIC_ACTIONS_INCLUDED_MINUTES:-}" && -n "${INTERWEAVE_ACTIONS_INCLUDED_MINUTES:-}" ]] \
    && export AGENT_FABRIC_ACTIONS_INCLUDED_MINUTES="$INTERWEAVE_ACTIONS_INCLUDED_MINUTES"
export AGENT_FABRIC_ACTIONS_INCLUDED_SETTING="${AGENT_FABRIC_ACTIONS_INCLUDED_SETTING:-INTERWEAVE_ACTIONS_INCLUDED_MINUTES}"
exec bash "$target" "$@"
