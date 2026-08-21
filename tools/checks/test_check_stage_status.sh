#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/test_check_stage_status.sh
#
# Self-test for check_stage_status.sh. Builds throwaway trees and asserts
# the guard's verdict on each, because a guard that cannot fail is a
# guard that passes silently-green.

set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GUARD="$HERE/check_stage_status.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

fails=0
ok()   { printf '  \342\234\223 %s\n' "$1"; }
bad()  { printf '  \342\234\227 %s\n' "$1"; fails=$((fails + 1)); }

# Build a tree with a given status and three prose files.
make_tree() {
  local dir="$1" status="$2" readme="$3" impl="$4" claude="$5" comment="${6:-}"
  mkdir -p "$dir"
  {
    printf '[workspace]\nmembers = ["xtask"]\n\n'
    printf '[workspace.metadata.interweave]\n'
    [ -n "$comment" ] && printf '# %s\n' "$comment"
    printf 'status = "%s"\n' "$status"
  } > "$dir/Cargo.toml"
  printf '%s\n' "$readme" > "$dir/README.md"
  printf '%s\n' "$impl"   > "$dir/IMPLEMENTATION.md"
  printf '%s\n' "$claude" > "$dir/CLAUDE.md"
}

expect() {
  local name="$1" dir="$2" want="$3"
  bash "$GUARD" --root "$dir" >/dev/null 2>&1
  local got=$?
  if [ "$got" -eq "$want" ]; then ok "$name"; else bad "$name (exit $got, wanted $want)"; fi
}

echo "check_stage_status: an agreeing tree passes"
make_tree "$TMP/good" "stage-3-persistence" \
  "Stage 3 is open." "Stage 3 is open." "Stage 3 is open."
expect "all three agree" "$TMP/good" 0

echo "check_stage_status: a stale prose statement fails"
make_tree "$TMP/stale" "stage-3-persistence" \
  "Stage 1 is open." "Stage 3 is open." "Stage 3 is open."
expect "README names the wrong stage" "$TMP/stale" 1

echo "check_stage_status: a file that states no stage fails"
make_tree "$TMP/silent" "stage-3-persistence" \
  "Nothing about stages here." "Stage 3 is open." "Stage 3 is open."
expect "README states no stage at all" "$TMP/silent" 1

echo "check_stage_status: the drift that actually happened"
# The value was updated and the comment above it was not — exactly the
# state the repository was left in after Stage 3 opened.
make_tree "$TMP/comment" "stage-3-persistence" \
  "Stage 3 is open." "Stage 3 is open." "Stage 3 is open." \
  "Stage 0 of the canonical plan: xtask is its only member."
expect "the comment contradicts its own field" "$TMP/comment" 1

echo "check_stage_status: the no-\`is\` phrasing is recognised on both sides"
# The positive check accepts "Stage 3 open" as well as "Stage 3 is
# open", and the README uses that shorter form. The exclusion scan
# recognised only the longer one, so a file could name TWO different open
# stages and pass — over the phrasing the repository actually uses.
make_tree "$TMP/shortform" "stage-3-persistence" \
  "Stage 3 open." "Stage 3 is open." "Stage 3 is open."
expect "the short form alone still passes" "$TMP/shortform" 0

make_tree "$TMP/shortstale" "stage-3-persistence" \
  "Stage 3 open. Stage 1 open." "Stage 3 is open." "Stage 3 is open."
expect "two open stages in the short form fails" "$TMP/shortstale" 1

make_tree "$TMP/mixedstale" "stage-3-persistence" \
  "Stage 3 is open. Stage 1 open." "Stage 3 is open." "Stage 3 is open."
expect "mixing the phrasings does not hide it" "$TMP/mixedstale" 1

echo "check_stage_status: \"opened\" is prose, not a claim about the open stage"
make_tree "$TMP/opened" "stage-3-persistence" \
  "Stage 3 is open. Stage 1 opened long ago." "Stage 3 is open." "Stage 3 is open."
expect "a past-tense mention is not a stale claim" "$TMP/opened" 0

echo "check_stage_status: a prose roster fails"
make_tree "$TMP/roster" "stage-3-persistence" \
  "Stage 3 is open. Its members are \`xtask\` the command runner and tests/support." \
  "Stage 3 is open." "Stage 3 is open."
expect "README restates the member list" "$TMP/roster" 1

echo "check_stage_status: a malformed status is an invocation error"
mkdir -p "$TMP/bad"
printf '[workspace.metadata.interweave]\nstatus = "whatever"\n' > "$TMP/bad/Cargo.toml"
: > "$TMP/bad/README.md"; : > "$TMP/bad/IMPLEMENTATION.md"; : > "$TMP/bad/CLAUDE.md"
expect "status is not stage-N-slug" "$TMP/bad" 2

echo "check_stage_status: a missing tree is an invocation error"
expect "no such directory" "$TMP/does-not-exist" 2

echo "check_stage_status: --help works and says nothing about failure"
if bash "$GUARD" --help | grep -q "single source of truth"; then
  ok "--help prints the help block"
else
  bad "--help did not print the help block"
fi

echo "check_stage_status: the real repository passes its own guard"
expect "this repository" "$HERE/../.." 0

if [ "$fails" -gt 0 ]; then
  printf '\ntest_check_stage_status: %d assertion(s) failed.\n' "$fails" >&2
  exit 1
fi
printf '\ntest_check_stage_status: OK — all assertions passed.\n'
