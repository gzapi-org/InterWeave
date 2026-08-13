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
          for t in tools/gh/test_*.sh tools/checks/test_*.sh; do bash "$t"; done
EOF
}

printf 'test_check_guards_are_wired\n'

# ── a wired, self-tested guard passes ────────────────────────────────────
R="$TMP/clean"; make_tree "$R"
[ "$(run_code "$R")" = "0" ] && ok "a wired, self-tested guard passes" || bad "should pass: $(run "$R")"

# ── a guard no workflow runs ─────────────────────────────────────────────
# The original defect: committed, hand-verified, invoked by nothing.
R="$TMP/unwired"; make_tree "$R"
printf '#!/usr/bin/env bash\n'  > "$R/tools/checks/check_orphan.sh"
printf '#!/usr/bin/env bash\n'  > "$R/tools/checks/test_check_orphan.sh"
out="$(run "$R")"
[ "$(run_code "$R")" = "1" ] && ok "an unwired guard exits 1" || bad "unwired guard should fail"
[[ "$out" == *"check_orphan.sh"*"cannot fail a pull request"* ]] \
    && ok "  and says it cannot fail a PR" || bad "should explain the consequence"

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
