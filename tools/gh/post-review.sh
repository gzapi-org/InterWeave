#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/gh/post-review.sh
#
# >>> help
# post-review lives in agent-fabric, the control plane checked out beside
# this working copy: runtime/github/post-review.sh. It posts THE review
# of a PR — the review class's blind review — as a review object at the
# head whose first line is `<!-- agent-fabric-review v1 -->`, which
# pr-review-status.sh counts. New here on 2026-09-25, with the retirement
# of the automated reviewer (agent-fabric
# docs/2026-09-20-the-review-class-is-the-review.md).
#
# This file only locates that script and hands it the arguments and
# stdin untouched (the review body arrives on stdin and must not be
# altered on its way through). It refuses, loudly, when agent-fabric is
# not beside this working copy — nothing falls back to a stale copy.
#
#   tools/gh/post-review.sh --help      # the real script's help
# <<< help
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(git -C "$here" rev-parse --show-toplevel 2>/dev/null || { cd "$here/../.." && pwd; })"
fabric="${AGENT_FABRIC_ROOT:-$root/../agent-fabric}"
target="$fabric/runtime/github/post-review.sh"
[[ -f "$target" ]] || {
    echo "post-review: agent-fabric not found at $fabric (expected beside this working copy, as projects/agent-fabric); set AGENT_FABRIC_ROOT or check it out. See CLAUDE.md §9, agent-fabric beside the checkout." >&2
    exit 2
}
exec bash "$target" "$@"
