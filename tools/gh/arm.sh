#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/gh/arm.sh
#
# >>> help
# arm lives in agent-fabric, the control plane checked out beside
# this working copy: runtime/github/arm.sh. It arms auto-merge on a PR
# with the gates CLAUDE.md §9 puts before the arming applied by the
# tool, not by memory: open and not a draft, this session's branch, no
# AWAITING-SUPPLY without its range line, a security-boundary change
# only with the review class's review of the CURRENT head and no
# unresolved thread (and the owner's word on one only under eight work
# commits), and the count rule (under eight work commits only on the
# owner's word). InterWeave's
# security-boundary paths are agent-fabric's
# projects/interweave/integration/gh/arm.json, found from this clone's
# remote; a path it misses is a change there, proposed to
# fabric-coordinator.
#
#   tools/gh/arm.sh <n> --basis "<one line>"
#                   [--boundary | --no-boundary "<why>" --waiver <message-id|seq>]
#                   [--any-owner] [--dry-run]
#
# This file only locates that script and hands it the arguments and
# stdin untouched. It refuses, loudly, when agent-fabric is not beside
# this working copy — nothing falls back to a stale copy.
#
#   tools/gh/arm.sh --help      # the real script's help: every gate and exit code
# <<< help
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tools/gh/fabric-root.sh
. "$here/fabric-root.sh"
fabric="$(interweave_fabric_root "$here")"
target="$fabric/runtime/github/arm.sh"
[[ -f "$target" ]] || {
    echo "arm: agent-fabric not found at $fabric (expected beside this working copy, as projects/agent-fabric); set AGENT_FABRIC_ROOT or check it out. See CLAUDE.md §9, agent-fabric beside the checkout." >&2
    exit 2
}
exec bash "$target" "$@"
