#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/test_check_license_headers.sh
#
# Self-test for check_license_headers.sh. Builds a throwaway git repo per
# case under $TMPDIR and runs the checker against it with --root, so the
# assertions never depend on the state of the real tree.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECK="$SCRIPT_DIR/check_license_headers.sh"

pass=0
fail=0

ok()   { printf '  ✓ %s\n' "$1"; pass=$((pass + 1)); }
bad()  { printf '  ✗ %s\n' "$1" >&2; fail=$((fail + 1)); }

# make_repo <dir> — an initialised repo with one committed, compliant file.
make_repo() {
    local d="$1"
    mkdir -p "$d"
    git -C "$d" init -q -b main
    git -C "$d" config user.email test@example.invalid
    git -C "$d" config user.name "Test"
    printf '#!/usr/bin/env bash\n# SPDX-License-Identifier: Apache-2.0\necho hi\n' > "$d/good.sh"
    git -C "$d" add -A
    git -C "$d" commit -q -m "seed"
}

# Output is captured into a variable, never piped into `grep -q`: with
# `pipefail` set, grep -q closes the pipe early, the checker dies of
# EPIPE, and the pipeline reports failure on output that was correct.
run_check() { bash "$CHECK" --root "$1" 2>&1; }
run_code()  { bash "$CHECK" --root "$1" >/dev/null 2>&1; printf '%s' "$?"; }

# Built from parts so this file carries no literal foreign declaration.
# The checker scans the whole tree, tests included, and a suite that
# trips its own subject teaches everyone to reach for an exemption.
TAG="SPDX-License-""Identifier"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

printf 'test_check_license_headers\n'

# ── a compliant tree passes ───────────────────────────────────────────────
R="$TMP/clean"; make_repo "$R"
[ "$(run_code "$R")" = "0" ] && ok "compliant tree exits 0" || bad "compliant tree should exit 0"

# ── a source file with no SPDX header is a violation ─────────────────────
R="$TMP/missing"; make_repo "$R"
printf '#!/usr/bin/env bash\necho no header\n' > "$R/bare.sh"
[ "$(run_code "$R")" = "1" ] && ok "missing header exits 1" || bad "missing header should exit 1"
out="$(run_check "$R")"
[[ "$out" == *"bare.sh"* ]] && ok "  and names the offending file" || bad "should name bare.sh"

# ── an SPDX tag naming another licence is a violation ────────────────────
R="$TMP/foreign"; make_repo "$R"
printf '#!/usr/bin/env bash\n# %s: GPL-3.0\n' "$TAG" > "$R/gpl.sh"
out="$(run_check "$R")"
[[ "$out" == *"GPL-3.0"* ]] && ok "foreign SPDX tag is reported with its licence" || bad "should report GPL-3.0"

# ── proprietary wording is a violation even in a non-source file ─────────
R="$TMP/prop"; make_repo "$R"
# Assembled from parts so this test file does not itself carry the
# phrase it is testing for — the checker scans the whole tree, tests
# included, and a self-tripping suite teaches everyone to exempt things.
printf 'Copyright (c) 2026 Someone. All %s reserved.\n' rights > "$R/NOTES.md"
[ "$(run_code "$R")" = "1" ] && ok "proprietary wording in markdown exits 1" || bad "markdown proprietary notice should fail"

# ── markdown WITHOUT a header is fine — prose carries no banner ───────────
R="$TMP/prose"; make_repo "$R"
printf '# A document\n\nNo licence banner here.\n' > "$R/DOC.md"
[ "$(run_code "$R")" = "0" ] && ok "markdown needs no SPDX header" || bad "markdown should not require a header"

# ── a foreign tag BELOW an added Apache header is still caught ───────────
# The shape every real import takes: prepend the Apache header, leave the
# original declaration further down. A first-match-only scan sees Apache
# and passes, defeating the check in exactly the case it exists for.
R="$TMP/mixed"; make_repo "$R"
printf '#!/usr/bin/env bash\n# SPDX-License-Identifier: Apache-2.0\n#\n# vendored from upstream\n# %s: GPL-3.0\necho hi\n' "$TAG" > "$R/mixed.sh"
[ "$(run_code "$R")" = "1" ] && ok "a second, foreign tag below an Apache header is caught" || bad "mixed headers should fail"
out="$(run_check "$R")"
[[ "$out" == *"GPL-3.0"* ]] && ok "  and names the foreign licence" || bad "should name GPL-3.0"

# ── an SPDX tag quoted mid-sentence in prose is not a foreign licence ────
R="$TMP/prose-tag"; make_repo "$R"
printf 'Files carry `SPDX-License-Identifier: Apache-2.0` in their opening lines, and that is checked.\n' > "$R/DOC.md"
[ "$(run_code "$R")" = "0" ] && ok "a quoted SPDX tag in prose is read as the token only" \
    || bad "prose quoting the tag should not be a violation"

# ── LICENSE itself is never scanned ──────────────────────────────────────
R="$TMP/license"; make_repo "$R"
printf 'Apache License\n\nCopyright [yyyy] [name of copyright owner]\nAll %s reserved.\n' rights > "$R/LICENSE"
[ "$(run_code "$R")" = "0" ] && ok "LICENSE is exempt from the scan" || bad "LICENSE should not be scanned"

# ── an exemption silences both checks, and only for the listed path ──────
R="$TMP/exempt"; make_repo "$R"
mkdir -p "$R/tools/checks"
printf '#!/usr/bin/env bash\n# %s: MIT\n' "$TAG" > "$R/vendored.sh"
printf '# provenance: upstream MIT snippet\nvendored.sh\n' > "$R/tools/checks/license_exempt.txt"
printf '# SPDX-License-Identifier: Apache-2.0\n' >> "$R/tools/checks/license_exempt.txt"
[ "$(run_code "$R")" = "0" ] && ok "an exempt path is skipped" || bad "exempt path should pass"
printf '#!/usr/bin/env bash\n# %s: MIT\n' "$TAG" > "$R/other.sh"
[ "$(run_code "$R")" = "1" ] && ok "  and the exemption does not cover its neighbours" || bad "non-exempt MIT file should fail"

# ── untracked-but-not-ignored files are scanned; ignored ones are not ────
R="$TMP/untracked"; make_repo "$R"
printf '#!/usr/bin/env bash\necho new\n' > "$R/brand-new.sh"
[ "$(run_code "$R")" = "1" ] && ok "an uncommitted new file is still scanned" || bad "untracked file should be scanned"
printf 'brand-new.sh\n' > "$R/.gitignore"
[ "$(run_code "$R")" = "0" ] && ok "an ignored file is not scanned" || bad "ignored file should be skipped"

# ── the checker survives its own scan ────────────────────────────────────
bash "$CHECK" --root "$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)" >/dev/null 2>&1
[ $? -le 1 ] && ok "runs against the real repository without erroring out" || bad "real-repo run errored (exit 2)"

# ── usage errors are exit 2, not 1 ───────────────────────────────────────
bash "$CHECK" --root /nonexistent-path-xyz >/dev/null 2>&1
[ "$?" = "2" ] && ok "a bad --root exits 2" || bad "bad --root should exit 2"
bash "$CHECK" --nope >/dev/null 2>&1
[ "$?" = "2" ] && ok "an unknown option exits 2" || bad "unknown option should exit 2"
help_out="$(bash "$CHECK" --help 2>/dev/null)"
[[ "$help_out" == *"FOREIGN"* ]] && ok "--help prints the help block" || bad "--help should print help"

printf '\n'
if [ "$fail" -gt 0 ]; then
    printf 'test_check_license_headers: %d passed, %d FAILED.\n' "$pass" "$fail" >&2
    exit 1
fi
printf 'test_check_license_headers: OK — all %d assertions passed.\n' "$pass"
