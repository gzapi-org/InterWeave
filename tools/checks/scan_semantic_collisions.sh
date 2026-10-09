#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/scan_semantic_collisions.sh
#
# >>> help
# Detect SEMANTIC collisions between parallel contributions that a
# textually-clean git merge cannot flag.
#
# Sessions coordinate only through `origin` (CLAUDE.md §Concurrent
# sessions) and allocate sequence numbers independently, so two branches
# can each mint the same ADR number in DIFFERENT files, or write the
# byte-identical heading into the same file, and merge with zero
# conflicts — the merged tree is broken while git reports success.
#
# Checks, over the merged tree:
#   1. ADR file numbers under architecture/adr/ are unique. Two files
#      claiming `0049-` make every "ADR-0049" cross-reference ambiguous
#      and the supersession chain unreadable.
#   2. No ADR contains two identical amendment headings. Amendments are
#      cited by heading; byte-identical headings from parallel sessions
#      must be disambiguated, or every reference to one of them points
#      at both.
#
# On a hit, the fix is to RENUMBER YOUR OWN (newer) entry past the one
# that reached origin first — never to delete or renumber work that
# already landed.
#
# Run it after folding origin/main into your branch and before pushing;
# that is the moment the collision exists and the moment it is cheapest
# to fix.
#
# The scan lives in agent-fabric, the control plane checked out beside
# this working copy: runtime/github/scan-semantic-collisions.sh, the same for every
# project, run with InterWeave's rules there
# (projects/interweave/integration/gh/collisions.json), which this file names. InterWeave's own copy lived here until then; its
# suite, test_scan_semantic_collisions.sh, was the port's oracle. This file prints this
# help itself, and otherwise only locates the scan, names this working
# copy as --root (an explicit --root after it wins), and hands over. It
# refuses with exit 2 when agent-fabric is not beside this working copy
# — nothing falls back to a stale copy.
#
# Options:
#   --root <dir>   scan this repository instead of the one containing
#                  this script
#   -h, --help     this text
#
# Exit codes:
#   0  no collisions
#   1  one or more collisions found
#   2  invocation problem (expected directories missing; or no agent-fabric)
# <<< help

set -uo pipefail
for a in "$@"; do
    case "$a" in -h|--help) sed -n '/^# >>> help$/,/^# <<< help$/p' "$0" | sed -e '1d' -e '$d' -e 's/^# \{0,1\}//'; exit 0 ;; esac
done
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tools/gh/fabric-root.sh
. "$here/../gh/fabric-root.sh"
fabric="$(interweave_fabric_root "$here")"
target="$fabric/runtime/github/scan-semantic-collisions.sh"
rules="$fabric/projects/interweave/integration/gh/collisions.json"
[[ -f "$target" && -f "$rules" ]] || {
    echo "scan_semantic_collisions: agent-fabric not found at $fabric (expected beside this working copy, as projects/agent-fabric, carrying runtime/github/scan-semantic-collisions.sh and InterWeave's collisions.json); set AGENT_FABRIC_ROOT or update it. See CLAUDE.md §9, agent-fabric beside the checkout." >&2
    exit 2
}
# The rules are named rather than found from the working copy's remote: a
# test's sandbox has none the fabric knows. An explicit AGENT_FABRIC_COLLISIONS_CONFIG wins.
export AGENT_FABRIC_COLLISIONS_CONFIG="${AGENT_FABRIC_COLLISIONS_CONFIG:-$rules}"
exec bash "$target" --root "$(cd -- "$here/../.." && pwd)" "$@"
