#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/test_check_dependencies.sh
#
# Self-test for check_dependencies.sh.
#
# The interesting assertions are the two the wrapper exists for: a policy
# VIOLATION and a MISSING TOOL must be different answers. cargo-deny
# exits non-zero for both, and a check that conflates them teaches people
# to ignore it.
#
# Violations are produced by handing the real graph a deliberately
# impossible policy, rather than by synthesising a workspace: a synthetic
# tree would need its own dependency resolution, and the thing under test
# is this repository's graph.

set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GUARD="$HERE/check_dependencies.sh"
ROOT="$(cd "$HERE/../.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

fails=0
ok()  { printf '  \342\234\223 %s\n' "$1"; }
bad() { printf '  \342\234\227 %s\n' "$1"; fails=$((fails + 1)); }

have_deny() { command -v cargo-deny >/dev/null 2>&1 || cargo deny --version >/dev/null 2>&1; }

expect() {
  local name="$1" want="$2"; shift 2
  "$@" >/dev/null 2>&1
  local got=$?
  if [ "$got" -eq "$want" ]; then ok "$name"; else bad "$name (exit $got, wanted $want)"; fi
}

echo "check_dependencies: --help works"
if bash "$GUARD" --help | grep -q "ALLOW-LIST"; then
  ok "--help prints the help block"
else
  bad "--help did not print the help block"
fi

echo "check_dependencies: a missing policy file is an invocation error"
expect "no such config" 2 bash "$GUARD" --config "$TMP/absent.toml"

echo "check_dependencies: an unknown argument is an invocation error"
expect "unexpected argument" 2 bash "$GUARD" --nonsense

if ! have_deny; then
  echo "check_dependencies: cargo-deny absent — running the absence path only"
  expect "reports exit 2, not a policy failure" 2 bash "$GUARD"
  printf '\ntest_check_dependencies: OK — cargo-deny not installed, absence path verified.\n'
  exit "$fails"
fi

echo "check_dependencies: the real repository satisfies its own policy"
expect "deny.toml passes" 0 bash "$GUARD"

echo "check_dependencies: a policy violation is exit 1, not exit 2"
# An empty licence allow-list rejects every crate in the graph, including
# this repository's own. Nothing else about the tree changes.
cat > "$TMP/impossible.toml" <<'CFG'
[graph]
all-features = true
exclude-dev = false
[licenses]
version = 2
allow = []
CFG
expect "empty allow-list is a violation" 1 bash "$GUARD" --config "$TMP/impossible.toml"

echo "check_dependencies: a missing cargo-deny is exit 2, not exit 1"
# A PATH that still has bash and cargo but NOT cargo-deny, since
# cargo-deny installs to ~/.cargo/bin. An empty PATH would only prove
# that `bash` cannot be found.
#
# If the tool were merely unavailable and this reported 1, a broken
# environment would read as a dependency problem and someone would go
# looking for the cause in Cargo.toml.
# CARGO_HOME too: cargo resolves a subcommand from $CARGO_HOME/bin
# whatever PATH says, so clearing PATH alone leaves `cargo deny` working
# and the assertion passes for the wrong reason.
mkdir -p "$TMP/nocargo"
expect "absent tool is an environment answer" 2 \
  env PATH="/usr/bin:/bin" CARGO_HOME="$TMP/nocargo" bash "$GUARD"

echo "check_dependencies: the policy names a licence the graph actually uses"
# Guards against an allow-list that drifted into aspiration: every entry
# should correspond to something present, or the list has stopped
# describing this project.
if (cd "$ROOT" && cargo deny check licenses 2>&1) | grep -q "license-not-encountered"; then
  bad "deny.toml allows a licence nothing in the graph uses"
else
  ok "every allowed licence is one the graph resolves to"
fi

if [ "$fails" -gt 0 ]; then
  printf '\ntest_check_dependencies: %d assertion(s) failed.\n' "$fails" >&2
  exit 1
fi
printf '\ntest_check_dependencies: OK — all assertions passed.\n'
