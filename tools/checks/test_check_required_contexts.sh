#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# Self-test for check_required_contexts.sh.
#
# Each case builds the disagreement it claims to catch, because a guard
# asserted only against the real tree passes for as long as the tree
# happens to be right.

set -uo pipefail

SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
GUARD="$SCRIPT_DIR/check_required_contexts.sh"
[ -f "$GUARD" ] || { echo "test: guard not found at $GUARD" >&2; exit 1; }

failures=0
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

ok()  { echo "  ✓ $1"; }
bad() { echo "  ✗ $1" >&2; failures=$((failures + 1)); }

# A workflow with the given job names, plus step names at step depth so
# the guard has to distinguish them.
workflow() {
    local root="$1"; shift
    mkdir -p "$root/.github/workflows"
    {
        echo "name: CI"
        echo "jobs:"
        for job in "$@"; do
            echo "  ${job// /_}:"
            echo "    name: $job"
            echo "    steps:"
            echo "      - name: Checkout"
            echo "      - name: Run the thing"
        done
    } > "$root/.github/workflows/ci.yml"
}

contract() {
    local root="$1"; shift
    local bolded=""
    for c in "$@"; do bolded="$bolded **\`$c\`**,"; done
    printf '# CLAUDE.md\n\nIt reports %d contexts, which are the job `name:` values verbatim:%s and nothing else.\n' \
        "$#" "${bolded%,}" > "$root/CLAUDE.md"
}

run_code() { bash "$GUARD" --root "$1" >/dev/null 2>&1; printf '%s' "$?"; }
run()      { bash "$GUARD" --root "$1" 2>&1; }

printf 'test_check_required_contexts\n'

echo "check_required_contexts: agreement passes"
R="$TMP/agree"; mkdir -p "$R"
workflow "$R" "rust" "tree checks" "tool self-tests"
contract "$R" "rust" "tree checks" "tool self-tests"
[ "$(run_code "$R")" = "0" ] && ok "matching sets are fine" || bad "should pass: $(run "$R")"

echo "check_required_contexts: the drift that actually happened"
# CI grew a `rust` job running fmt, clippy, and every workspace test,
# and the paragraph telling a reader what gates main went on saying two.
R="$TMP/undercount"; mkdir -p "$R"
workflow "$R" "rust" "tree checks" "tool self-tests"
contract "$R" "tree checks" "tool self-tests"
out="$(run "$R")"
[ "$(run_code "$R")" = "1" ] && ok "an unlisted job fails" || bad "should exit 1"
case "$out" in *rust*) ok "  and names the job that is missing from the document" ;;
               *) bad "  must name it: $out" ;; esac

echo "check_required_contexts: a renamed job is the dangerous direction"
# Renaming un-gates main silently: the ruleset keeps requiring a context
# nothing reports, and the queue waits forever.
R="$TMP/renamed"; mkdir -p "$R"
workflow "$R" "rust" "tree-checks" "tool self-tests"
contract "$R" "rust" "tree checks" "tool self-tests"
out="$(run "$R")"
[ "$(run_code "$R")" = "1" ] && ok "a rename fails" || bad "should exit 1"
case "$out" in *"waits forever"*) ok "  and says why it matters" ;;
               *) bad "  must explain the consequence: $out" ;; esac

echo "check_required_contexts: step names are not contexts"
# Every job has steps called `name:` too. Counting those would make the
# guard fail on every correct workflow, which is a guard nobody keeps.
R="$TMP/steps"; mkdir -p "$R"
workflow "$R" "rust"
contract "$R" "rust"
[ "$(run_code "$R")" = "0" ] && ok "step names are ignored" || bad "should pass: $(run "$R")"

echo "check_required_contexts: a document that stopped saying anything fails"
# Deleting the sentence must not read as agreement.
R="$TMP/silent"; mkdir -p "$R"
workflow "$R" "rust" "tree checks"
printf '# CLAUDE.md\n\nNothing about CI here.\n' > "$R/CLAUDE.md"
[ "$(run_code "$R")" = "1" ] && ok "a missing statement is a failure, not a pass" \
    || bad "should exit 1: $(run "$R")"

echo "check_required_contexts: invocation errors are distinct from findings"
[ "$(bash "$GUARD" --nonsense >/dev/null 2>&1; printf '%s' "$?")" = "2" ] \
    && ok "an unknown option exits 2" || bad "unknown option should exit 2"
R="$TMP/empty"; mkdir -p "$R"
[ "$(run_code "$R")" = "2" ] && ok "a tree with no workflow exits 2" \
    || bad "missing workflow should exit 2"

echo "check_required_contexts: --help works and says nothing about the tree"
bash "$GUARD" --help >/dev/null 2>&1 && ok "--help exits 0" || bad "--help should exit 0"

echo
if [ "$failures" -gt 0 ]; then
    echo "test_check_required_contexts: FAILED — $failures assertion(s)" >&2
    exit 1
fi
echo "test_check_required_contexts: OK — all assertions passed."
