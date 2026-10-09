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
# A stub shellcheck: the fake tools never call it, and the guard only asks
# that one exists and prints its version, so the suite needs none on the
# host (the no-shellcheck case builds its own PATH without it).
printf '#!/usr/bin/env bash\necho ShellCheck\necho "version: stub"\n' > "$SANDBOX/bin/shellcheck"
chmod +x "$SANDBOX/bin/shellcheck"

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

# The cache is keyed by digest: a pin whose digest changes for the same
# version must not reuse the old binary. The stub still serves the old
# tarball, so the new digest fails it — and curl must have been asked.
rm -f "$SANDBOX/curl-calls"
expect 2 "a digest change for the same version fetches again (and fails on the old bytes)" ACTIONLINT_SHA256="$(printf 'f%.0s' {1..64})"
[[ -e "$SANDBOX/curl-calls" ]] && pass "  and it did download" || bad "a changed digest reused the cached binary"

fresh; expect 1 "an actionlint finding (its exit 1) fails" FAKE_ACTIONLINT_RC=1
for rc in 10 11 12 13 14; do
    fresh; expect 1 "a zizmor finding (its exit $rc) fails" FAKE_ZIZMOR_RC=$rc
done
fresh; expect 2 "actionlint unable to run (its exit 3) is exit 2, not a finding" FAKE_ACTIONLINT_RC=3
fresh; expect 2 "zizmor unable to run (its exit 2) is exit 2, not a pass" FAKE_ZIZMOR_RC=2

# The gate that matters: a download that is not the pinned release.
fresh; expect 2 "a checksum mismatch is exit 2" ACTIONLINT_SHA256="$(printf '0%.0s' {1..64})"
[[ ! -e "$SANDBOX/ran-actionlint" ]] && pass "  and the mismatched binary never ran" || bad "a binary that failed its checksum was run"
[[ -z "$(find "$SANDBOX/cache" -name actionlint -type f 2>/dev/null)" ]] && pass "  and nothing was cached" || bad "a mismatched binary was left in the cache"

fresh; touch "$SANDBOX/curl-fails"; expect 2 "a failed download is exit 2"

# An archive whose member is not the tool: exit 2, and nothing half-made
# left in the cache for a later run to reuse.
mkdir -p "$SANDBOX/dist/wrong"; echo x > "$SANDBOX/dist/wrong/README"
tar -czf "$SANDBOX/dist/wrong.tgz" -C "$SANDBOX/dist/wrong" README
WRONG_SHA="$(sha256sum "$SANDBOX/dist/wrong.tgz" | cut -d' ' -f1)"
fresh; expect 2 "an archive without the tool at its root is exit 2" ACTIONLINT_URL="$SANDBOX/dist/wrong.tgz" ACTIONLINT_SHA256="$WRONG_SHA"
[[ -z "$(find "$SANDBOX/cache" -name actionlint 2>/dev/null)" ]] && pass "  and nothing is left in the cache" || bad "a failed extract left a file in the cache"

# A repository with no workflows is a failure to check, not a pass.
mv "$SANDBOX/repo/.github" "$SANDBOX/github.away"
fresh; expect 2 "no .github/workflows is exit 2"
mv "$SANDBOX/github.away" "$SANDBOX/repo/.github"

# No shellcheck: actionlint would skip every run: script and still pass.
# The stub is moved aside for this one case and restored after it.
fresh
mv "$SANDBOX/bin/shellcheck" "$SANDBOX/shellcheck.away"
mkdir -p "$SANDBOX/noshell"
for t in bash env tar sha256sum mktemp mkdir rm cp sed cut tr touch find cat dirname; do
    ln -sf "$(command -v "$t")" "$SANDBOX/noshell/$t"
done
out="$(env PATH="$SANDBOX/bin:$SANDBOX/noshell" INTERWEAVE_TOOL_CACHE="$SANDBOX/cache" \
    ACTIONLINT_URL="$SANDBOX/dist/actionlint.tgz" ACTIONLINT_SHA256="$AL_SHA" \
    ZIZMOR_URL="$SANDBOX/dist/zizmor.tgz" ZIZMOR_SHA256="$ZZ_SHA" bash "$UNDER_TEST" --root "$SANDBOX/repo" 2>&1)"; got=$?
[[ "$got" -eq 2 && "$out" == *"shellcheck is not on PATH"* ]] && pass "no shellcheck is exit 2, named (exit $got)" \
    || bad "no shellcheck — wanted 2 naming shellcheck, got $got" "$out"
mv "$SANDBOX/shellcheck.away" "$SANDBOX/bin/shellcheck"

# The hand-over to agent-fabric's check-workflows-lint.sh: every case above ran the
# real one (CI points AGENT_FABRIC_ROOT at its pinned checkout); these pin
# what the hand-over itself promises, against a recording stub.
hcheck() { if eval "$2"; then pass "$1"; else bad "$1" "$hout"; fi; }
hstub="$(mktemp -d)"
mkdir -p "$hstub/fabric/runtime/github" "$hstub/fabric/projects/interweave/integration/gh"
cat > "$hstub/fabric/runtime/github/check-workflows-lint.sh" <<'STUB'
#!/usr/bin/env bash
printf 'config=%s cache=%s' "${NONE-<unset>}" "${AGENT_FABRIC_TOOL_CACHE-<unset>}"; printf ' [%s]' "$@"; echo
exit 3
STUB
# shellcheck disable=SC2034 # read in hcheck's eval'd conditions
hrun() { hout="$(env -u AGENT_FABRIC_NONE -u AGENT_FABRIC_TOOL_CACHE -u INTERWEAVE_TOOL_CACHE AGENT_FABRIC_ROOT="$hstub/fabric" "$@" 2>&1)"; hrc=$?; }
# shellcheck disable=SC2034 # read in hcheck's eval'd conditions
hrepo="$( cd -- "$SCRIPT_DIR/../.." && pwd )"
hrun bash "$UNDER_TEST" --root /elsewhere
hcheck "the fabric's check-workflows-lint gets this working copy as --root, the caller's arguments after it" '[[ $hrc -eq 3 && "$hout" == *" [--root] [$hrepo] [--root] [/elsewhere]" ]]'
hrun bash "$UNDER_TEST" --help
hcheck "--help is this file's own block, not the fabric's" '[[ $hrc -eq 0 && "$hout" != *"[--help]"* && "$hout" == *"agent-fabric"* ]]'
hrun env INTERWEAVE_TOOL_CACHE=/iw bash "$UNDER_TEST"
hcheck "INTERWEAVE_TOOL_CACHE is the fabric's cache" '[[ "$hout" == *"cache=/iw "* ]]'
hrun env INTERWEAVE_TOOL_CACHE=/iw AGENT_FABRIC_TOOL_CACHE=/af bash "$UNDER_TEST"
hcheck "an explicit AGENT_FABRIC_TOOL_CACHE wins" '[[ "$hout" == *"cache=/af "* ]]'
hout="$(AGENT_FABRIC_ROOT="$hstub/none" bash "$UNDER_TEST" x 2>&1)"
# shellcheck disable=SC2034 # read in hcheck's eval'd conditions
hrc=$?
hcheck "no agent-fabric: exit 2, naming where it looked" '[[ $hrc -eq 2 && "$hout" == *"agent-fabric not found at $hstub/none"* ]]'
rm -rf "$hstub"

echo
if (( fails > 0 )); then
    echo "test_check_workflows_lint: $fails failure(s)" >&2
    exit 1
fi
echo "test_check_workflows_lint: all cases passed"
