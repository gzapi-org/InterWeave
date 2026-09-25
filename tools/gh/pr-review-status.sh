#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/gh/pr-review-status.sh
#
# >>> help
# pr-review-status lives in agent-fabric, the control plane checked out
# beside this working copy: runtime/github/pr-review-status.sh. It answers
# whether THE HEAD of a PR has been reviewed — by the review class's blind
# review (a review object whose first line is `<!-- agent-fabric-review v1 -->`,
# posted by post-review.sh) or by an independent reviewer — and assumes no
# automated reviewer.
#
# InterWeave kept its own copy until 2026-09-25, when the automated
# reviewer was retired (agent-fabric
# docs/2026-09-20-the-review-class-is-the-review.md): that copy counted
# only the retired reviewer, so the review class's reviews read as "head
# reviewed? : no" (#113). `--automated-only` and the decline paths went
# with it. The call goes straight to runtime/github/ — InterWeave injects
# no settings, so it has no integration entry (fabric-coordinator's
# ruling); the day it needs one, this file is repointed in that change.
#
# This file only locates that script and hands it the arguments and
# stdin untouched. It refuses, loudly, when agent-fabric is not beside
# this working copy — nothing falls back to a stale copy.
#
#   tools/gh/pr-review-status.sh --help      # the real script's help
# <<< help
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(git -C "$here" rev-parse --show-toplevel 2>/dev/null || { cd "$here/../.." && pwd; })"
fabric="${AGENT_FABRIC_ROOT:-$root/../agent-fabric}"
target="$fabric/runtime/github/pr-review-status.sh"
[[ -f "$target" ]] || {
    echo "pr-review-status: agent-fabric not found at $fabric (expected beside this working copy, as projects/agent-fabric); set AGENT_FABRIC_ROOT or check it out. See CLAUDE.md - the control plane." >&2
    exit 2
}
exec bash "$target" "$@"
