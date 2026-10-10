#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/gh/test_fabric-root.sh
#
# Behavioural tests for fabric-root.sh — where the tools/gh hand-overs
# find agent-fabric:
#
#   1. AGENT_FABRIC_ROOT wins; empty is unset
#   2. from the clone: ../agent-fabric beside it
#   3. from a git worktree of the clone, wherever it is: still beside the
#      CLONE — the defect this file exists for (every hand-over exited 2
#      from .claude/worktrees/…); a forwarder run there reaches it
#   4. outside a repository: two above the script, as before
#
# Exit codes:
#   0  all assertions passed
#   1  one or more assertions failed

set -uo pipefail

SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
UNDER_TEST="$SCRIPT_DIR/fabric-root.sh"
[[ -f "$UNDER_TEST" ]] || { echo "test: $UNDER_TEST not found" >&2; exit 1; }

failures=0
pass() { echo "  ok   $1"; }
fail() { echo "  FAIL $1"; [[ -n "${2:-}" ]] && printf '%s\n' "$2" | sed 's/^/       /'; failures=$((failures+1)); }

SANDBOX="$(realpath -- "$(mktemp -d)")"
trap 'rm -rf "$SANDBOX"' EXIT

# A clone with the real fabric-root.sh and actions-health.sh,
# agent-fabric beside it, and a worktree of it somewhere else entirely.
P="$SANDBOX/projects"; C="$P/interweave"
mkdir -p "$C/tools/gh" "$P/agent-fabric/tools/fabric/github"
cp "$UNDER_TEST" "$SCRIPT_DIR/actions-health.sh" "$C/tools/gh/"
printf '#!/usr/bin/env bash\necho "the sibling fabric"\n' > "$P/agent-fabric/tools/fabric/github/actions_health.py"
# The module stub is a shell script; a fake interpreter runs it with bash.
printf '#!/bin/sh\nexec bash "$@"\n' > "$SANDBOX/fake-python"; chmod +x "$SANDBOX/fake-python"
export AGENT_FABRIC_PYTHON="$SANDBOX/fake-python"
git -C "$C" init -q && git -C "$C" -c user.name=t -c user.email=t@t add -A && git -C "$C" -c user.name=t -c user.email=t@t commit -qm init
mkdir -p "$C/.claude/worktrees"
git -C "$C" worktree add -q --detach "$C/.claude/worktrees/agent-x" HEAD
git -C "$C" worktree add -q --detach "$SANDBOX/elsewhere/wt" HEAD

# Compared as resolved paths: the answer is <clone>/../agent-fabric.
# shellcheck source=tools/gh/fabric-root.sh
root_from() { realpath -m -- "$(unset AGENT_FABRIC_ROOT; . "$UNDER_TEST"; interweave_fabric_root "$1")"; }
want="$P/agent-fabric"

echo "fabric-root: AGENT_FABRIC_ROOT wins; empty is unset"
got="$(AGENT_FABRIC_ROOT=/opt/fabric bash -c ". '$UNDER_TEST'; interweave_fabric_root '$C/tools/gh'")"
[[ "$got" == /opt/fabric ]] && pass "AGENT_FABRIC_ROOT is used as given" || fail "env" "$got"
got="$(realpath -m -- "$(AGENT_FABRIC_ROOT='' bash -c ". '$UNDER_TEST'; interweave_fabric_root '$C/tools/gh'")")"
[[ "$got" == "$want" ]] && pass "an empty AGENT_FABRIC_ROOT is no setting" || fail "empty env" "$got"

echo "fabric-root: beside the clone, from the clone and from any worktree of it"
[[ "$(root_from "$C/tools/gh")" == "$want" ]] && pass "from the clone" || fail "clone" "$(root_from "$C/tools/gh")"
[[ "$(root_from "$C/.claude/worktrees/agent-x/tools/gh")" == "$want" ]] && pass "from a subagent's worktree under .claude/worktrees/" || fail "worktree inside" "$(root_from "$C/.claude/worktrees/agent-x/tools/gh")"
[[ "$(root_from "$SANDBOX/elsewhere/wt/tools/gh")" == "$want" ]] && pass "from a worktree outside the clone" || fail "worktree outside" "$(root_from "$SANDBOX/elsewhere/wt/tools/gh")"
out="$(cd "$C/.claude/worktrees/agent-x" && env -u AGENT_FABRIC_ROOT bash tools/gh/actions-health.sh 2>&1)"; rc=$?
[[ $rc -eq 0 && "$out" == "the sibling fabric" ]] && pass "  and a forwarder run in the worktree reaches it" || fail "forwarder in worktree" "rc=$rc $out"

echo "fabric-root: outside a repository, two above the script"
mkdir -p "$SANDBOX/plain/tools/gh"
[[ "$(root_from "$SANDBOX/plain/tools/gh")" == "$SANDBOX/agent-fabric" ]] && pass "no repository: <dir>/../.. stands for the top level" || fail "no repo" "$(root_from "$SANDBOX/plain/tools/gh")"

echo "fabric-root: without the pinned Python, each module hand-over says how to install it"
# Every forwarder that runs a tools/fabric/github module gets its Python from
# interweave_fabric_python. A fabric carrying each module and its rules, and
# AGENT_FABRIC_PYTHON at a file that does not exist: the install message, and
# the exit the forwarder documents (127; actions-health 2, "could not find out").
NOPY="$SANDBOX/nopy-fabric"; mkdir -p "$NOPY/tools/fabric/github" "$NOPY/projects/interweave/integration/gh"
for m in actions_health run_suite guards_wired workflows_lint semantic_collisions; do : > "$NOPY/tools/fabric/github/$m.py"; done
printf '{}' > "$NOPY/projects/interweave/integration/gh/guards.json"
printf '{}' > "$NOPY/projects/interweave/integration/gh/collisions.json"
REPO="$(cd -- "$SCRIPT_DIR/../.." && pwd)"
for case in tools/gh/actions-health.sh:2 tools/checks/run_suite.sh:127 tools/checks/check_guards_are_wired.sh:127 \
            tools/checks/check_workflows_lint.sh:127 tools/checks/scan_semantic_collisions.sh:127; do
    f="${case%%:*}"; want="${case##*:}"
    out="$(AGENT_FABRIC_ROOT="$NOPY" AGENT_FABRIC_PYTHON="$SANDBOX/no-such-python" bash "$REPO/$f" x 2>&1 </dev/null)"; rc=$?
    [[ $rc -eq $want && "$out" == *"pinned Python is not installed at $SANDBOX/no-such-python"* ]] \
        && pass "$f: exit $want, the install message" || fail "$f without the Python (want $want)" "rc=$rc $out"
done

echo
if (( failures )); then echo "test_fabric-root: $failures assertion(s) FAILED"; exit 1; fi
echo "test_fabric-root: OK — all assertions passed."
