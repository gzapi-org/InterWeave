#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/test_check_guards_are_wired.sh
#
# Self-test for check_guards_are_wired.sh. Each case builds a miniature
# tools/ + .github/workflows/ tree under $TMPDIR, so no assertion depends
# on the real repository — except the one that deliberately checks it.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECK="$SCRIPT_DIR/check_guards_are_wired.sh"

pass=0
fail=0
ok()  { printf '  ✓ %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf '  ✗ %s\n' "$1" >&2; fail=$((fail + 1)); }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# Captured, never piped into `grep -q`: with pipefail a `grep -q` that
# exits early kills the producer with EPIPE and turns correct output into
# a failed pipeline.
run()      { bash "$CHECK" --root "$1" 2>&1; }
run_code() { bash "$CHECK" --root "$1" >/dev/null 2>&1; printf '%s' "$?"; }

# make_tree <root> — one guard, its self-test, and a workflow running both.
make_tree() {
    local r="$1"
    mkdir -p "$r/tools/checks" "$r/tools/gh" "$r/.github/workflows"
    printf '#!/usr/bin/env bash\necho guard\n'      > "$r/tools/checks/check_thing.sh"
    printf '#!/usr/bin/env bash\necho selftest\n'   > "$r/tools/checks/test_check_thing.sh"
    cat > "$r/.github/workflows/ci.yml" <<'EOF'
name: CI
jobs:
  a:
    steps:
      - run: bash tools/checks/check_thing.sh
      - run: |
          for t in tools/gh/test_*.sh tools/checks/test_*.sh; do bash tools/checks/run_suite.sh "$t"; done
EOF
}

printf 'test_check_guards_are_wired\n'

# ── a wired, self-tested guard passes ────────────────────────────────────
R="$TMP/clean"; make_tree "$R"
[ "$(run_code "$R")" = "0" ] && ok "a wired, self-tested guard passes" || bad "should pass: $(run "$R")"

# ── a self-test loop that runs its suites bare ───────────────────────────
# Bare, an assertion calling an undefined helper passes; the loop must run
# each suite through tools/checks/run_suite.sh. Multi-line too.
R="$TMP/bareloop"; make_tree "$R"
sed -i 's#bash tools/checks/run_suite.sh "\$t"#bash "$t"#' "$R/.github/workflows/ci.yml"
out="$(run "$R")"
[ "$(run_code "$R")" = "1" ] && ok "a self-test loop run bare exits 1" || bad "a bare self-test loop should fail: $out"
[[ "$out" == *"without tools/checks/run_suite.sh"* ]] \
    && ok "  and names the runner" || bad "should name run_suite.sh: $out"
R="$TMP/multiloop"; make_tree "$R"
cat >> "$R/.github/workflows/ci.yml" <<'EOF'
      - run: |
          for t in tools/ci/test_*.sh; do
            if bash tools/checks/run_suite.sh "$t"; then :; else exit 1; fi
          done
      - run: |
          for t in tools/gh/test_*.sh; do
            bash "$t"
          done
EOF
out="$(run "$R")"
[ "$(run_code "$R")" = "1" ] && [[ "$out" == *"for t in tools/gh/test_*.sh"* && "$out" != *"for t in tools/ci/test_*.sh"* ]] \
    && ok "  a multi-line loop is read to its done: the bare one fails, the wrapped one passes" \
    || bad "multi-line loops misread: $out"

# ── a guard no workflow runs ─────────────────────────────────────────────
# The original defect: committed, hand-verified, invoked by nothing.
R="$TMP/unwired"; make_tree "$R"
printf '#!/usr/bin/env bash\n'  > "$R/tools/checks/check_orphan.sh"
printf '#!/usr/bin/env bash\n'  > "$R/tools/checks/test_check_orphan.sh"
out="$(run "$R")"
[ "$(run_code "$R")" = "1" ] && ok "an unwired guard exits 1" || bad "unwired guard should fail"
[[ "$out" == *"check_orphan.sh"*"cannot fail a pull request"* ]] \
    && ok "  and says it cannot fail a PR" || bad "should explain the consequence"

# ── a guard a workflow only MENTIONS, in a comment ───────────────────────
# A step deleted while a comment elsewhere still names the guard: nothing
# runs it. Both comment shapes -- a YAML note and a commented-out command
# inside a run block -- must leave it unwired.
R="$TMP/comment-only"; make_tree "$R"
printf '#!/usr/bin/env bash\n'  > "$R/tools/checks/check_orphan.sh"
printf '#!/usr/bin/env bash\n'  > "$R/tools/checks/test_check_orphan.sh"
cat >> "$R/.github/workflows/ci.yml" <<'YAML'
      # check_orphan.sh used to run here.
      - run: |
          # bash tools/checks/check_orphan.sh
          echo later
YAML
out="$(run "$R")"
[ "$(run_code "$R")" = "1" ] && ok "a guard named only in comments is unwired" \
    || bad "a comment should not wire a guard: $out"

# ── a guard whose name appears only inside its self-test's name ──────────
# `test_check_orphan.sh` contains `check_orphan.sh`; running the self-test
# never runs the guard.
R="$TMP/suffix-only"; make_tree "$R"
printf '#!/usr/bin/env bash\n'  > "$R/tools/checks/check_orphan.sh"
printf '#!/usr/bin/env bash\n'  > "$R/tools/checks/test_check_orphan.sh"
cat >> "$R/.github/workflows/ci.yml" <<'YAML'
      - run: bash tools/checks/test_check_orphan.sh
YAML
out="$(run "$R")"
[ "$(run_code "$R")" = "1" ] && ok "a guard named only as part of its self-test's name is unwired" \
    || bad "a longer name should not wire a guard: $out"

# ── a guard with no self-test ────────────────────────────────────────────
R="$TMP/untested"; make_tree "$R"
printf '#!/usr/bin/env bash\n' > "$R/tools/checks/check_bare.sh"
sed -i 's|- run: bash tools/checks/check_thing.sh|- run: bash tools/checks/check_thing.sh\n      - run: bash tools/checks/check_bare.sh|' \
    "$R/.github/workflows/ci.yml"
out="$(run "$R")"
[[ "$out" == *"no self-test beside it"* ]] && ok "a guard with no self-test is reported" || bad "should require a self-test"

# ── an exemption silences the self-test requirement ──────────────────────
R="$TMP/exempt"; make_tree "$R"
printf '#!/usr/bin/env bash\n' > "$R/tools/checks/check_bare.sh"
sed -i 's|- run: bash tools/checks/check_thing.sh|- run: bash tools/checks/check_thing.sh\n      - run: bash tools/checks/check_bare.sh|' \
    "$R/.github/workflows/ci.yml"
printf 'tools/checks/check_bare.sh\n' > "$R/tools/checks/selftest_exempt.txt"
[ "$(run_code "$R")" = "0" ] && ok "an exempt guard needs no self-test" || bad "exemption should silence it: $(run "$R")"

# ── an UNWIRED SELF-TEST is the same defect one level up ─────────────────
# A suite that passes locally and gates nothing.
R="$TMP/unwired-suite"; make_tree "$R"
rm "$R/.github/workflows/ci.yml"
cat > "$R/.github/workflows/ci.yml" <<'EOF'
name: CI
jobs:
  a:
    steps:
      - run: bash tools/checks/check_thing.sh
EOF
out="$(run "$R")"
[[ "$out" == *"test_check_thing.sh"*"gates nothing"* ]] \
    && ok "an unwired self-test is reported" || bad "should report the unwired suite"

# ── a glob in the workflow counts as wiring what it covers ───────────────
# The suites are invoked through `for t in tools/gh/test_*.sh`, so a
# literal-name-only match would report every one of them as unwired.
R="$TMP/glob"; make_tree "$R"
printf '#!/usr/bin/env bash\n' > "$R/tools/gh/test_helper.sh"
[ "$(run_code "$R")" = "0" ] && ok "a test_*.sh glob wires the suites it expands to" \
    || bad "glob should count as wiring: $(run "$R")"

# ── tools/gh helpers need a self-test but need not run in CI ─────────────
# They are interactive PR helpers a person invokes; their SUITES are what
# CI runs.
R="$TMP/ghhelper"; make_tree "$R"
printf '#!/usr/bin/env bash\n' > "$R/tools/gh/pr-thing.sh"
printf '#!/usr/bin/env bash\n' > "$R/tools/gh/test_pr-thing.sh"
[ "$(run_code "$R")" = "0" ] && ok "a gh helper with a suite passes without its own CI step" \
    || bad "gh helper should not need a direct workflow reference: $(run "$R")"

R="$TMP/ghhelper-bare"; make_tree "$R"
printf '#!/usr/bin/env bash\n' > "$R/tools/gh/pr-thing.sh"
out="$(run "$R")"
[[ "$out" == *"pr-thing.sh"*"no self-test"* ]] && ok "  but still needs a self-test" || bad "gh helper must be self-tested"

# ── tools/ci is scanned, and its scripts must run in a workflow ─────────
# A CI session wrapper is there only to be run by a job; one no job runs,
# or whose suite no glob reaches, is the same unreachable code.
R="$TMP/ci-wired"; make_tree "$R"
mkdir -p "$R/tools/ci"
printf '#!/usr/bin/env bash\n' > "$R/tools/ci/with_thing.sh"
printf '#!/usr/bin/env bash\n' > "$R/tools/ci/test_with_thing.sh"
cat >> "$R/.github/workflows/ci.yml" <<'YAML'
      - run: bash tools/ci/with_thing.sh cargo test
      - run: for t in tools/ci/test_*.sh; do bash tools/checks/run_suite.sh "$t"; done
YAML
[ "$(run_code "$R")" = "0" ] && ok "a tools/ci script run by a job, its suite globbed, passes" \
    || bad "a wired tools/ci script should pass: $(run "$R")"

R="$TMP/ci-unwired"; make_tree "$R"
mkdir -p "$R/tools/ci"
printf '#!/usr/bin/env bash\n' > "$R/tools/ci/with_thing.sh"
printf '#!/usr/bin/env bash\n' > "$R/tools/ci/test_with_thing.sh"
out="$(run "$R")"
[[ "$out" == *"tools/ci/with_thing.sh"*"cannot fail a pull request"* ]] \
    && ok "a tools/ci script no job runs is reported" || bad "an unwired tools/ci script should be reported: $out"
[[ "$out" == *"tools/ci/test_with_thing.sh"*"gates nothing"* ]] \
    && ok "  and so is its unwired suite" || bad "an unwired tools/ci suite should be reported: $out"

R="$TMP/ci-untested"; make_tree "$R"
mkdir -p "$R/tools/ci"
printf '#!/usr/bin/env bash\n' > "$R/tools/ci/with_thing.sh"
printf '      - run: bash tools/ci/with_thing.sh\n' >> "$R/.github/workflows/ci.yml"
out="$(run "$R")"
[[ "$out" == *"tools/ci/with_thing.sh"*"no self-test"* ]] && ok "a tools/ci script needs a self-test" \
    || bad "a tools/ci script must be self-tested: $out"

# ── a python guard is covered too ────────────────────────────────────────
R="$TMP/py"; make_tree "$R"
printf '#!/usr/bin/env python3\n' > "$R/tools/checks/validate_thing.py"
out="$(run "$R")"
[[ "$out" == *"validate_thing.py"* ]] && ok "a .py guard is checked, not only .sh" || bad "should cover python guards"

# ── the real repository is wired ─────────────────────────────────────────
REAL="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel 2>/dev/null)"
if [ -n "$REAL" ]; then
    [ "$(run_code "$REAL")" = "0" ] && ok "the real repository's guards are all wired" \
        || bad "real repo has unwired guards: $(run "$REAL")"
fi

# ── tools/host/android: person-run scripts whose self-tests CI runs ──────
# A host script is like a tools/gh one: it needs a self-test, the self-test
# needs a workflow, and the script itself needs none.
R="$TMP/host"; make_tree "$R"; mkdir -p "$R/tools/host/android"
printf '#!/usr/bin/env bash\n' > "$R/tools/host/android/provision.sh"
printf '#!/usr/bin/env bash\n' > "$R/tools/host/android/test_provision.sh"
sed -i 's#tools/checks/test_\*.sh; do#tools/checks/test_*.sh tools/host/android/test_*.sh; do#' "$R/.github/workflows/ci.yml"
[ "$(run_code "$R")" = "0" ] && ok "a host script with a wired self-test passes, though no workflow runs the script" \
    || bad "a host script should need no workflow of its own: $(run "$R")"
rm "$R/tools/host/android/test_provision.sh"
out="$(run "$R")"
[[ "$(run_code "$R")" = "1" && "$out" == *"tools/host/android/provision.sh: no self-test beside it"* ]] \
    && ok "a host script with no self-test fails, named" || bad "a host script's missing self-test went unnoticed: $out"
printf '#!/usr/bin/env bash\n' > "$R/tools/host/android/test_provision.sh"
sed -i 's# tools/host/android/test_\*.sh; do#; do#' "$R/.github/workflows/ci.yml"
out="$(run "$R")"
[[ "$(run_code "$R")" = "1" && "$out" == *"tools/host/android/test_provision.sh: no workflow runs it"* ]] \
    && ok "a host self-test no workflow runs fails, named" || bad "an unwired host self-test went unnoticed: $out"

# ── usage ────────────────────────────────────────────────────────────────
[ "$(run_code "$TMP/nothing-here")" = "2" ] && ok "a tree with no workflows exits 2" || bad "missing workflows should exit 2"
help_out="$(bash "$CHECK" --help 2>/dev/null)"
[[ "$help_out" == *"passes silently-green"* ]] && ok "--help prints the help block" || bad "--help should print help"

printf '\n'
if [ "$fail" -gt 0 ]; then
    printf 'test_check_guards_are_wired: %d passed, %d FAILED.\n' "$pass" "$fail" >&2
    exit 1
fi
printf 'test_check_guards_are_wired: OK — all %d assertions passed.\n' "$pass"
