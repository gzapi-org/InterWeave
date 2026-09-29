#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/test_check_actions_pinned_by_sha.sh
#
# Self-test for check_actions_pinned_by_sha.sh.
#
# The cases that matter are the ones that LOOK pinned: an exact release tag
# (`@v4.38.2` is still a tag its owner can move), a 7-character short SHA
# (GitHub resolves it, and it can collide), and a correct SHA with no
# version comment (runs fine, and Dependabot silently stops updating it).
set -u

SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
UNDER_TEST="$SCRIPT_DIR/check_actions_pinned_by_sha.sh"
[[ -f "$UNDER_TEST" ]] || { echo "test: not found: $UNDER_TEST" >&2; exit 1; }

SHA=3d3c42e5aac5ba805825da76410c181273ba90b1
DIGEST=$(printf 'a%.0s' {1..64})

fails=0
pass() { echo "  ok:   $1"; }
bad()  { echo "  FAIL: $1" >&2; printf '%s\n' "${2:-}" | sed 's/^/        /' >&2
         fails=$((fails + 1)); }

# expect <exit> <name> <workflow-body> [<file under .github>]
expect() {
    local want="$1" name="$2" body="$3" path="${4:-workflows/ci.yml}" root out got
    root="$(mktemp -d)"; mkdir -p "$root/.github/workflows" "$(dirname "$root/.github/$path")"
    printf '%s\n' "$body" > "$root/.github/$path"
    out="$(bash "$UNDER_TEST" --root "$root" 2>&1)"; got=$?
    rm -rf "$root"
    [[ "$got" -eq "$want" ]] && pass "$name (exit $got)" \
        || bad "$name — wanted $want, got $got" "$out"
}

step() { printf 'jobs:\n  a:\n    steps:\n%s\n' "$1"; }

expect 0 "a SHA with its version comment passes (list-item form)" \
"$(step "      - uses: actions/checkout@$SHA # v7.0.1")"

expect 0 "a SHA with its version comment passes (key form)" \
"$(step "      - if: true
        uses: actions/checkout@$SHA # v7.0.1")"

expect 0 "a sub-path action pinned by SHA passes" \
"$(step "      - uses: github/codeql-action/init@$SHA # v4.38.2")"

expect 0 "a quoted SHA pin passes" \
"$(step "      - uses: 'actions/checkout@$SHA' # v7.0.1")"

expect 1 "a major tag is rejected" \
"$(step "      - uses: actions/checkout@v7")"

# Looks exact; still a tag.
expect 1 "an exact release tag is rejected" \
"$(step "      - uses: github/codeql-action/init@v4.38.2")"

expect 1 "a branch is rejected" \
"$(step "      - uses: some/action@main")"

expect 1 "a short SHA is rejected" \
"$(step "      - uses: actions/checkout@3d3c42e # v7.0.1")"

expect 1 "an upper-case SHA is rejected (GitHub's refs are lower-case)" \
"$(step "      - uses: actions/checkout@${SHA^^} # v7.0.1")"

expect 1 "no ref at all is rejected" \
"$(step "      - uses: actions/checkout")"

# Runs, and silently stops receiving updates.
expect 1 "a SHA with no version comment is rejected" \
"$(step "      - uses: actions/checkout@$SHA")"

expect 1 "a SHA whose comment is not a version is rejected" \
"$(step "      - uses: actions/checkout@$SHA # pinned")"

expect 0 "a local action and a local reusable workflow are exempt" \
"$(printf 'jobs:\n  a:\n    uses: ./.github/workflows/_web.yml\n  b:\n    steps:\n      - uses: ./.github/actions/setup\n')"

expect 0 "a docker action pinned by digest passes" \
"$(step "      - uses: docker://docker.io/library/alpine@sha256:$DIGEST")"

expect 1 "a docker action by tag is rejected" \
"$(step "      - uses: docker://docker.io/library/alpine:3.20")"

expect 0 "a commented-out tag pin is not a use" \
"$(step "      # - uses: actions/checkout@v4
      - uses: actions/checkout@$SHA # v7.0.1")"

expect 1 "a composite action under .github/actions is checked too" \
"$(printf 'runs:\n  using: composite\n  steps:\n    - uses: actions/setup-node@v7\n')" \
"actions/setup/action.yml"

expect 1 "one bad use among good ones fails the file" \
"$(step "      - uses: actions/checkout@$SHA # v7.0.1
      - uses: actions/setup-node@v7")"

# No workflows is an invocation problem, not a pass.
root="$(mktemp -d)"
bash "$UNDER_TEST" --root "$root" >/dev/null 2>&1; got=$?
rm -rf "$root"
[[ "$got" -eq 2 ]] && pass "no workflow directory is exit 2" \
    || bad "no workflow directory — wanted 2, got $got"

echo
if (( fails > 0 )); then
    echo "test_check_actions_pinned_by_sha: $fails failure(s)" >&2
    exit 1
fi
echo "test_check_actions_pinned_by_sha: all cases passed"
