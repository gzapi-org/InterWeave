#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# .claude/statusline.sh
#
# Claude Code status line: model · host · clone-dir · git branch.
#
# It surfaces the three facts that identify a session under the branch
# attribution model in CLAUDE.md §9: branches are named
# <hostname>/<clone-dir>/<type>/<desc>, so host and clone are what tell
# you whose branch you are standing on — and the branch itself is what
# §9 says to check BEFORE the first commit of a task, not after a push
# is rejected. Having it permanently on screen is cheaper than the
# `git rev-parse --abbrev-ref HEAD` that step exists to force.
#
# Reads the session JSON on stdin. Runs locally, uses no API token, and
# must stay fast: it is executed on every render. Wired up by the
# `statusLine` entry in .claude/settings.json.

input=$(cat)

dir=$(printf '%s' "$input" | jq -r '.workspace.current_dir // empty')
[ -z "$dir" ] && dir="$PWD"
model=$(printf '%s' "$input" | jq -r '.model.display_name // "Claude"')
host=$(hostname -s 2>/dev/null || hostname 2>/dev/null || echo "?")
clone=$(basename "$dir")
branch=$(git -C "$dir" branch --show-current 2>/dev/null)

# `main` is protected and cannot take a direct commit (§9), so standing
# on it is nearly always the start-of-task state that wants a branch cut
# rather than a place to work. Marking it costs one comparison.
mark=""
[ "$branch" = "main" ] && mark=" ⚠"

printf '[%s] 🖥  %s 📁 %s 🌿 %s%s' "$model" "$host" "$clone" "${branch:-detached}" "$mark"
