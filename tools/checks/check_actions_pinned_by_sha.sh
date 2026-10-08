#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/check_actions_pinned_by_sha.sh
#
# >>> help
# Every third-party action a workflow `uses:` is pinned to a full commit
# SHA, with its release as a trailing comment (exit 0), or not (exit 1);
# 2 when it could not check.
#
# The check lives in agent-fabric, beside this working copy:
# policies/check_actions_pinned_by_sha.py, run on the fleet's pinned
# Python. This file locates both and hands the arguments over, adding
# --root <this working tree's top level> unless one is given. In CI the
# tree-checks job checks agent-fabric out at .agent-fabric/fabric-ref
# and exports AGENT_FABRIC_ROOT.
#
#   tools/checks/check_actions_pinned_by_sha.sh --help   # the check's own help
# <<< help
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tools/gh/fabric-root.sh
. "$here/../gh/fabric-root.sh"
fabric="$(interweave_fabric_root "$here")"
target="$fabric/policies/check_actions_pinned_by_sha.py"
[[ -f "$target" ]] || {
    echo "check_actions_pinned_by_sha: agent-fabric not found at $fabric (expected beside this working copy, as projects/agent-fabric, carrying policies/check_actions_pinned_by_sha.py); set AGENT_FABRIC_ROOT or update it. See CLAUDE.md §9, agent-fabric beside the checkout." >&2
    exit 2
}
py="${AGENT_FABRIC_PYTHON:-/usr/local/bin/fabric-python}"
[[ -x "$py" ]] || {
    echo "check_actions_pinned_by_sha: the fleet's pinned Python is not installed at $py; as root: /usr/bin/python3 $fabric/tools/fabric/python_pin.py install" >&2
    exit 2
}
args=("$@")
given=0
for a in "$@"; do case "$a" in --root|--root=*) given=1 ;; esac; done
if (( ! given )); then
    top="$(git -C "$here" rev-parse --show-toplevel 2>/dev/null || { cd "$here/../.." && pwd; })"
    args=(--root "$top" "${args[@]}")
fi
exec "$py" "$target" "${args[@]}"
