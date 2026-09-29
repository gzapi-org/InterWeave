#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/test_check_workflows_lint.sh
#
# Self-test for check_workflows_lint.sh.
#
# The tools are downloaded, so every case runs against a stub `curl` on
# PATH that serves tarballs built here, holding fake actionlint and zizmor
# whose exit codes the case chooses. What is under test is the guard's
# own logic: the checksum gate (a mismatch must never run the binary),
# the cache, the tools' exit codes mapped to finding / failure-to-run, and
# the refusal without shellcheck. The cases that matter most are the ones
# that must NOT pass: a tampered download, and a tool that crashed.
set -u

SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
UNDER_TEST="$SCRIPT_DIR/check_workflows_lint.sh"
[[ -f "$UNDER_TEST" ]] || { echo "test: not found: $UNDER_TEST" >&2; exit 1; }

fails=0
pass() { echo "  ok:   $1"; }
bad()  { echo "  FAIL: $1" >&2; printf '%s\n' "${2:-}" | sed 's/^/        /' >&2
         fails=$((fails + 1)); }

SANDBOX="$(mktemp -d)"
trap 'rm -rf "$SANDBOX"' EXIT
mkdir -p "$SANDBOX/repo/.github/workflows" "$SANDBOX/bin" "$SANDBOX/dist"
printf 'on: push\n' > "$SANDBOX/repo/.github/workflows/ci.yml"

# A fake tool: records that it ran, exits with $FAKE_<NAME>_RC.
fake_tool() {  # fake_tool <name>
    local up; up="$(tr '[:lower:]' '[:upper:]' <<<"$1")"
    mkdir -p "$SANDBOX/dist/$1"
    cat > "$SANDBOX/dist/$1/$1" <<EOF
#!/usr/bin/env bash
touch "$SANDBOX/ran-$1"
exit "\${FAKE_${up}_RC:-0}"
EOF
    chmod +x "$SANDBOX/dist/$1/$1"
    tar -czf "$SANDBOX/dist/$1.tgz" -C "$SANDBOX/dist/$1" "$1"
}
fake_tool actionlint
fake_tool zizmor
AL_SHA="$(sha256sum "$SANDBOX/dist/actionlint.tgz" | cut -d' ' -f1)"
ZZ_SHA="$(sha256sum "$SANDBOX/dist/zizmor.tgz" | cut -d' ' -f1)"

# curl stub: `curl … -o <out> <url>` copies the named tarball; counts calls.
cat > "$SANDBOX/bin/curl" <<EOF
#!/usr/bin/env bash
echo x >> "$SANDBOX/curl-calls"
[[ -e "$SANDBOX/curl-fails" ]] && exit 22
out=""; url=""
while (( \$# )); do case "\$1" in -o) out="\$2"; shift 2 ;; -*) shift ;; *) url="\$1"; shift ;; esac; done
cp "\$url" "\$out"
EOF
chmod +x "$SANDBOX/bin/curl"

# run [env…] — the guard against the sandbox repo, a fresh cache unless kept.
run() {
    env PATH="$SANDBOX/bin:$PATH" INTERWEAVE_TOOL_CACHE="$SANDBOX/cache" \
        ACTIONLINT_URL="$SANDBOX/dist/actionlint.tgz" ACTIONLINT_SHA256="$AL_SHA" \
        ZIZMOR_URL="$SANDBOX/dist/zizmor.tgz" ZIZMOR_SHA256="$ZZ_SHA" \
        "$@" bash "$UNDER_TEST" --root "$SANDBOX/repo" 2>&1
}
fresh() { rm -rf "$SANDBOX/cache" "$SANDBOX"/ran-* "$SANDBOX/curl-calls" "$SANDBOX/curl-fails"; }

# expect <exit> <name> [env…]
expect() {
    local want="$1" name="$2" out got; shift 2
    out="$(run "$@")"; got=$?
    [[ "$got" -eq "$want" ]] && pass "$name (exit $got)" || bad "$name — wanted $want, got $got" "$out"
}

echo "test_check_workflows_lint"

fresh; expect 0 "both tools clean passes"
[[ -e "$SANDBOX/ran-actionlint" && -e "$SANDBOX/ran-zizmor" ]] && pass "  and both tools actually ran" || bad "a tool did not run"

rm -f "$SANDBOX"/ran-* "$SANDBOX/curl-calls"
expect 0 "a second run uses the cache"
[[ ! -e "$SANDBOX/curl-calls" ]] && pass "  without downloading again" || bad "the cached tools were downloaded again"

fresh; expect 1 "an actionlint finding (its exit 1) fails" FAKE_ACTIONLINT_RC=1
fresh; expect 1 "a zizmor finding (its exit 13) fails" FAKE_ZIZMOR_RC=13
fresh; expect 2 "actionlint unable to run (its exit 3) is exit 2, not a finding" FAKE_ACTIONLINT_RC=3
fresh; expect 2 "zizmor unable to run (its exit 2) is exit 2, not a pass" FAKE_ZIZMOR_RC=2

# The gate that matters: a download that is not the pinned release.
fresh; expect 2 "a checksum mismatch is exit 2" ACTIONLINT_SHA256="$(printf '0%.0s' {1..64})"
[[ ! -e "$SANDBOX/ran-actionlint" ]] && pass "  and the mismatched binary never ran" || bad "a binary that failed its checksum was run"
[[ -z "$(find "$SANDBOX/cache" -name actionlint -type f 2>/dev/null)" ]] && pass "  and nothing was cached" || bad "a mismatched binary was left in the cache"

fresh; touch "$SANDBOX/curl-fails"; expect 2 "a failed download is exit 2"

# No shellcheck: actionlint would skip every run: script and still pass.
fresh
mkdir -p "$SANDBOX/noshell"
for t in bash env tar sha256sum mktemp mkdir rm cp sed cut tr touch find cat dirname; do
    ln -sf "$(command -v "$t")" "$SANDBOX/noshell/$t"
done
out="$(env PATH="$SANDBOX/bin:$SANDBOX/noshell" INTERWEAVE_TOOL_CACHE="$SANDBOX/cache" \
    ACTIONLINT_URL="$SANDBOX/dist/actionlint.tgz" ACTIONLINT_SHA256="$AL_SHA" \
    ZIZMOR_URL="$SANDBOX/dist/zizmor.tgz" ZIZMOR_SHA256="$ZZ_SHA" bash "$UNDER_TEST" --root "$SANDBOX/repo" 2>&1)"; got=$?
[[ "$got" -eq 2 && "$out" == *"shellcheck is not on PATH"* ]] && pass "no shellcheck is exit 2, named (exit $got)" \
    || bad "no shellcheck — wanted 2 naming shellcheck, got $got" "$out"

echo
if (( fails > 0 )); then
    echo "test_check_workflows_lint: $fails failure(s)" >&2
    exit 1
fi
echo "test_check_workflows_lint: all cases passed"
