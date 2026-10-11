#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/test_check_osv_exemptions.sh
#
# Self-test for check_osv_exemptions.py. Builds throwaway repository
# roots under $TMPDIR, each with Gradle lockfiles and an osv-scanner.toml,
# and asserts the check's verdict on each. A passing root is the positive
# control beside every failing one, so a check that fails everything
# cannot pass this test.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECK="$SCRIPT_DIR/check_osv_exemptions.py"
REAL="$(cd "$SCRIPT_DIR/../.." && pwd)"

pass=0
fail=0
ok()  { printf '  ✓ %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf '  ✗ %s\n' "$1" >&2; fail=$((fail + 1)); }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# Captured, never piped into `grep -q` -- pipefail plus an early-exiting
# grep turns correct output into a failed pipeline.
run()      { python3 "$CHECK" --root "$@" 2>&1; }
run_code() { python3 "$CHECK" --root "$@" >/dev/null 2>&1; printf '%s' "$?"; }

# A build with a lint-only artefact, a build-classpath one, and one that
# ships, as the lockfiles record them.
make_root() {
    local r="$1"
    mkdir -p "$r/apps/human-android/android/app"
    cat > "$r/apps/human-android/android/app/gradle.lockfile" <<'EOF'
# This is a Gradle generated file for dependency locking.
org.example:lint-only:1.0.0=androidLintTool
org.example:ships:2.0.0=androidLintTool,releaseRuntimeClasspath
empty=
EOF
    cat > "$r/apps/human-android/android/buildscript-gradle.lockfile" <<'EOF'
org.example:plugin-dep:3.0.0=classpath
empty=
EOF
}

# One entry, its reason built from the parts given; ignoreUntil last.
entry() {
    local id="$1" artefact="$2" configuration="$3" severity="$4" reviewed="$5" until="$6"
    cat <<EOF
[[IgnoredVulns]]
id = "$id"
ignoreUntil = $until
reason = """
artefact: $artefact
configuration: $configuration
severity: $severity
why: read only by the build on first-party input
changes: the plugin release that drops it
checked: AGP 9.4.1 $reviewed
reviewed: $reviewed
"""

EOF
}

GOOD="$TMP/good"
make_root "$GOOD"
{
    entry GHSA-aaaa-bbbb-cccc org.example:lint-only:1.0.0 androidLintTool MODERATE 2026-10-11 2027-01-09
    entry GHSA-dddd-eeee-ffff org.example:plugin-dep:3.0.0 classpath CRITICAL 2026-10-11 2026-11-10
} > "$GOOD/apps/human-android/android/osv-scanner.toml"

echo "positive controls"
code="$(run_code "$GOOD")"
[[ "$code" == 0 ]] && ok "a conforming file passes" || { bad "a conforming file fails (exit $code)"; run "$GOOD" >&2; }
NONE="$TMP/none"; make_root "$NONE"
out="$(run "$NONE")"; code="$(run_code "$NONE")"
[[ "$code" == 0 && "$out" == *"nothing is accepted"* ]] && ok "no file passes, saying so" || bad "no file: exit $code, $out"
code="$(run_code "$GOOD" --today 2026-11-10)"
[[ "$code" == 0 ]] && ok "--today on the last day passes" || bad "--today on the last day: exit $code"

# Each case: a name, a fragment the output must carry, and the file.
failing() {
    local name="$1" says="$2" extra="${3:-}"
    local r="$TMP/case-$((pass + fail))"
    make_root "$r"
    [[ -n "$extra" ]] && printf '%s\n' "$extra" >> "$r/apps/human-android/android/app/gradle.lockfile"
    cat > "$r/apps/human-android/android/osv-scanner.toml"
    local out code
    out="$(run "$r")"; code="$(run_code "$r")"
    if [[ "$code" == 1 && "$out" == *"$says"* ]]; then ok "$name"; else bad "$name: exit $code, wanted '$says' in: $out"; fi
}

echo "rule 1: only [[IgnoredVulns]]"
failing "a [[PackageOverrides]] entry fails" "only [[IgnoredVulns]]" <<'EOF'
[[PackageOverrides]]
name = "org.example:ships"
ignore = true
EOF

echo "rule 2: keys"
failing "an extra key fails" "keys other than" < <(entry GHSA-aaaa-bbbb-cccc org.example:lint-only:1.0.0 androidLintTool LOW 2026-10-11 2026-12-01 | sed 's/^id = /effectiveUntil = 1\nid = /')
failing "a missing ignoreUntil fails" "missing ['ignoreUntil']" < <(entry GHSA-aaaa-bbbb-cccc org.example:lint-only:1.0.0 androidLintTool LOW 2026-10-11 2026-12-01 | grep -v '^ignoreUntil')
failing "a repeated id fails" "already accepted" < <(
    entry GHSA-aaaa-bbbb-cccc org.example:lint-only:1.0.0 androidLintTool LOW 2026-10-11 2026-12-01
    entry GHSA-aaaa-bbbb-cccc org.example:plugin-dep:3.0.0 classpath LOW 2026-10-11 2026-12-01)

echo "rule 3: the reason's parts"
for part in artefact configuration severity why changes checked reviewed; do
    failing "a reason without \`$part:\` fails" "\`$part:\`" < <(entry GHSA-aaaa-bbbb-cccc org.example:lint-only:1.0.0 androidLintTool LOW 2026-10-11 2026-12-01 | grep -v "^$part:")
done
failing "a \`checked:\` with no date fails" "names no release and date" < <(entry GHSA-aaaa-bbbb-cccc org.example:lint-only:1.0.0 androidLintTool LOW 2026-10-11 2026-12-01 | sed 's/^checked: .*/checked: AGP 9.4.1/')
failing "a \`checked:\` with only a date fails" "names no release and date" < <(entry GHSA-aaaa-bbbb-cccc org.example:lint-only:1.0.0 androidLintTool LOW 2026-10-11 2026-12-01 | sed 's/^checked: .*/checked: 2026-10-11/')
failing "an unknown severity fails" "is none of" < <(entry GHSA-aaaa-bbbb-cccc org.example:lint-only:1.0.0 androidLintTool SEVERE 2026-10-11 2026-12-01)

echo "rule 4: the horizon"
failing "ignoreUntil on the review date fails" "is not after the review date" < <(entry GHSA-aaaa-bbbb-cccc org.example:lint-only:1.0.0 androidLintTool LOW 2026-10-11 2026-10-11)
failing "91 days for a MODERATE fails" "at most 90" < <(entry GHSA-aaaa-bbbb-cccc org.example:lint-only:1.0.0 androidLintTool MODERATE 2026-10-11 2027-01-10)
failing "31 days for a CRITICAL fails" "at most 30" < <(entry GHSA-aaaa-bbbb-cccc org.example:plugin-dep:3.0.0 classpath CRITICAL 2026-10-11 2026-11-11)
code="$(run_code "$GOOD" --today 2026-11-11)"
[[ "$code" == 1 ]] && ok "--today after an ignoreUntil fails" || bad "--today after an ignoreUntil: exit $code"

echo "rule 5: only a known build-only configuration"
failing "naming a RuntimeClasspath fails" "not a known build-only configuration" < <(entry GHSA-aaaa-bbbb-cccc org.example:ships:2.0.0 releaseRuntimeClasspath LOW 2026-10-11 2026-12-01)
failing "naming a CompileClasspath fails" "not a known build-only configuration" \
    "org.example:compiled:1.0.0=releaseCompileClasspath" \
    < <(entry GHSA-aaaa-bbbb-cccc org.example:compiled:1.0.0 releaseCompileClasspath LOW 2026-10-11 2026-12-01)
failing "naming coreLibraryDesugaring, dexed into the APK, fails" "not a known build-only configuration" \
    "com.android.tools:desugar_jdk_libs:2.0.0=coreLibraryDesugaring" \
    < <(entry GHSA-aaaa-bbbb-cccc com.android.tools:desugar_jdk_libs:2.0.0 coreLibraryDesugaring LOW 2026-10-11 2026-12-01)
failing "naming a configuration nobody classified fails" "not a known build-only configuration" \
    "org.example:lint-dep:1.0.0=lintChecks" \
    < <(entry GHSA-aaaa-bbbb-cccc org.example:lint-dep:1.0.0 lintChecks LOW 2026-10-11 2026-12-01)

echo "rule 6: the entry against the lockfiles"
failing "an artefact on no lockfile fails" "on no committed lockfile" < <(entry GHSA-aaaa-bbbb-cccc org.example:lint-only:9.9.9 androidLintTool LOW 2026-10-11 2026-12-01)
failing "a configuration the lockfiles do not give it fails" "but the lockfiles give" < <(entry GHSA-aaaa-bbbb-cccc org.example:plugin-dep:3.0.0 androidLintTool LOW 2026-10-11 2026-12-01)
failing "naming only some of the artefact's configurations fails" "but the lockfiles give" \
    "org.example:two-places:1.0.0=androidLintTool,kotlinCompilerClasspath" \
    < <(entry GHSA-aaaa-bbbb-cccc org.example:two-places:1.0.0 androidLintTool LOW 2026-10-11 2026-12-01)
failing "\"lint only\" about an artefact that also ships fails" "not build-only" < <(entry GHSA-aaaa-bbbb-cccc org.example:ships:2.0.0 androidLintTool LOW 2026-10-11 2026-12-01)
failing "a build-only version beside a shipped version of the same artefact fails" "lint-only:0.9.0" \
    "org.example:lint-only:0.9.0=releaseRuntimeClasspath" \
    < <(entry GHSA-aaaa-bbbb-cccc org.example:lint-only:1.0.0 androidLintTool LOW 2026-10-11 2026-12-01)
# The control for the two above: another build-only version passes.
CTRL="$TMP/ctrl"; make_root "$CTRL"
printf 'org.example:lint-only:0.9.0=kotlinCompilerClasspath\n' >> "$CTRL/apps/human-android/android/app/gradle.lockfile"
entry GHSA-aaaa-bbbb-cccc org.example:lint-only:1.0.0 androidLintTool LOW 2026-10-11 2026-12-01 > "$CTRL/apps/human-android/android/osv-scanner.toml"
code="$(run_code "$CTRL")"
[[ "$code" == 0 ]] && ok "a second, build-only version of the same artefact passes" || { bad "build-only second version: exit $code"; run "$CTRL" >&2; }

failing "an IgnoredVulns element that is not a table fails" "not a table" <<'EOF'
IgnoredVulns = ["GHSA-aaaa-bbbb-cccc"]
EOF

echo "could not run"
BROKEN="$TMP/broken"; make_root "$BROKEN"
printf '[[IgnoredVulns]\n' > "$BROKEN/apps/human-android/android/osv-scanner.toml"
code="$(run_code "$BROKEN")"
[[ "$code" == 2 ]] && ok "a file that is not TOML exits 2" || bad "not TOML: exit $code"

echo "the real tree"
code="$(python3 "$CHECK" --root "$REAL" >/dev/null 2>&1; printf '%s' "$?")"
[[ "$code" == 0 ]] && ok "this repository passes" || { bad "this repository fails (exit $code)"; python3 "$CHECK" --root "$REAL" >&2; }

echo
echo "test_check_osv_exemptions: $pass passed, $fail failed"
(( fail == 0 ))
