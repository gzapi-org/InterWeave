#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# tools/checks/check_workflows_lint.sh
#
# >>> help
# Does every workflow pass actionlint and zizmor?
#
#   tools/checks/check_workflows_lint.sh
#   tools/checks/check_workflows_lint.sh --root <dir>
#
# Every workflow passes actionlint (its run: scripts through shellcheck)
# and zizmor (the workflow security audit), with the release of each
# pinned HERE, once, for tree checks and for `cargo xtask checks` before a push.
#
# WHY ONE SCRIPT. The two tools used to be downloaded inline in
# the tree-checks job, so a session could learn of a finding only from a
# pushed run, and a local run would have had to copy the pins — two
# copies of a version drift, and the one CI uses is the one that counts.
#
# WHAT EACH CATCHES. actionlint: a run: script that does not parse (a
# broken script fails only on the path that reaches the broken line —
# a sibling repository's weekly report died on the one run it existed for), and shellcheck
# warnings. Info- and style-level notes are ignored: which fire varies with
# the shellcheck release. zizmor, offline: ${{ }} spliced into a script,
# over-broad permissions, persisted checkout tokens, spoofable bot checks,
# inherited secrets. A rule this repository decides against would go in a
# .github/zizmor.yml with its reason (there is none today: every rule is
# on); a single site is excused inline with `# zizmor: ignore[<rule>]`
# and its reason.
#
# THE TOOLS are release binaries, fetched once into the cache
# (INTERWEAVE_TOOL_CACHE, else $XDG_CACHE_HOME/interweave-tools, else
# ~/.cache/interweave-tools; CI points it at $RUNNER_TEMP), each checked
# against its sha256 before it is extracted or run. A cached binary is
# reused only from a directory named for its version and digest, so a
# pin bump fetches afresh. shellcheck comes from PATH: actionlint skips
# its script pass SILENTLY without one, so its absence is exit 2.
#
# Options:
#   --root <dir>   check this repository instead of the one containing
#                  this script
#   -h, --help     this text
#
# Exit codes:
#   0  both tools ran and found nothing
#   1  a finding (printed)
#   2  a tool could not be obtained or run (no network, a checksum
#      mismatch, no shellcheck) — nothing was checked
# <<< help

set -uo pipefail

SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
REPO_ROOT="$( cd -- "$SCRIPT_DIR/../.." && pwd )"
me="check_workflows_lint"

while [ $# -gt 0 ]; do
    case "$1" in
        -h|--help) sed -n '/^# >>> help$/,/^# <<< help$/p' "$0" | sed -e '1d' -e '$d' -e 's/^# \{0,1\}//'; exit 0 ;;
        --root)    [ $# -ge 2 ] || { echo "$me: --root needs a value" >&2; exit 2; }
                   REPO_ROOT="$2"; shift 2 ;;
        *)         echo "$me: unexpected argument: $1" >&2; exit 2 ;;
    esac
done

# The pins. Bump version and digest together; the digest is the release
# asset's (GitHub shows it on the release page; `sha256sum` the download).
# Each may be overridden from the environment: that is the self-test's
# seam, and it moves nothing silently — a version without its digest
# fails the checksum, and CI sets none of them.
ACTIONLINT_VERSION="${ACTIONLINT_VERSION:-1.7.12}"
ACTIONLINT_SHA256="${ACTIONLINT_SHA256:-8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8}"
ZIZMOR_VERSION="${ZIZMOR_VERSION:-1.30.1}"
ZIZMOR_SHA256="${ZIZMOR_SHA256:-e65324f4430c2717591937edcec90ccbefaf14c174f8ec9415e03ca875b46e1a}"
ACTIONLINT_URL="${ACTIONLINT_URL:-https://github.com/rhysd/actionlint/releases/download/v${ACTIONLINT_VERSION}/actionlint_${ACTIONLINT_VERSION}_linux_amd64.tar.gz}"
ZIZMOR_URL="${ZIZMOR_URL:-https://github.com/zizmorcore/zizmor/releases/download/v${ZIZMOR_VERSION}/zizmor-x86_64-unknown-linux-gnu.tar.gz}"

CACHE="${INTERWEAVE_TOOL_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/interweave-tools}"

# fetch <name> <version> <sha256> <url> — prints the binary's path.
fetch() {
    local name="$1" version="$2" sha="$3" url="$4" dir tmp
    dir="$CACHE/$name-$version-${sha:0:12}"
    if [[ -x "$dir/$name" ]]; then printf '%s\n' "$dir/$name"; return 0; fi
    mkdir -p "$dir" || { echo "$me: cannot create $dir" >&2; return 2; }
    tmp="$(mktemp "$dir/.download.XXXXXX")" || return 2
    if ! curl -fsSL --retry 3 --retry-all-errors -o "$tmp" "$url"; then
        rm -f "$tmp"; echo "$me: could not download $name $version ($url)" >&2; return 2
    fi
    if ! echo "$sha  $tmp" | sha256sum --check --strict --status; then
        rm -f "$tmp"
        echo "$me: $name $version does not match its pinned sha256 — not run" >&2
        return 2
    fi
    # Extracted beside the cache entry and moved into place, so an
    # interrupted run never leaves a truncated binary that the cache check
    # above would then reuse on every later run.
    local stage
    stage="$(mktemp -d "$dir/.extract.XXXXXX")" || { rm -f "$tmp"; return 2; }
    if ! tar -xzf "$tmp" -C "$stage" "$name" 2>/dev/null; then
        rm -rf "$tmp" "$stage"; echo "$me: $name is not at the root of its archive" >&2; return 2
    fi
    mv -f "$stage/$name" "$dir/$name" && rm -rf "$tmp" "$stage" || { rm -rf "$tmp" "$stage"; return 2; }
    printf '%s\n' "$dir/$name"
}

command -v shellcheck >/dev/null || {
    echo "$me: shellcheck is not on PATH; actionlint would lint no run: script (dnf install ShellCheck / apt-get install shellcheck)" >&2
    exit 2
}
actionlint="$(fetch actionlint "$ACTIONLINT_VERSION" "$ACTIONLINT_SHA256" "$ACTIONLINT_URL")" || exit 2
zizmor="$(fetch zizmor "$ZIZMOR_VERSION" "$ZIZMOR_SHA256" "$ZIZMOR_URL")" || exit 2

cd "$REPO_ROOT" || exit 2
[[ -d .github/workflows ]] || { echo "$me: no .github/workflows under $REPO_ROOT" >&2; exit 2; }

# Each tool's own exit codes tell a finding from a failure to run:
# actionlint 1 is findings (2 and 3 are usage and fatal errors), zizmor
# 10-14 are findings by severity. Anything else is exit 2 here, never a
# finding and never a pass.
bad=0
echo "$me: actionlint $ACTIONLINT_VERSION, $(shellcheck --version | sed -n 2p)"
"$actionlint" -no-color -oneline -ignore ':(info|style):'; rc=$?
case "$rc" in 0) ;; 1) bad=1 ;; *) echo "$me: actionlint could not run (exit $rc)" >&2; exit 2 ;; esac
echo "$me: zizmor $ZIZMOR_VERSION (offline)"
"$zizmor" --offline --format plain .github/; rc=$?
case "$rc" in 0) ;; 1[0-4]) bad=1 ;; *) echo "$me: zizmor could not run (exit $rc)" >&2; exit 2 ;; esac

if (( bad )); then
    echo
    echo "$me: fix the finding, or excuse ONE site inline with its reason"
    echo "(# zizmor: ignore[<rule>]); a rule this repository decides against"
    echo "goes in a .github/zizmor.yml with its reason."
    exit 1
fi
echo "$me: OK — every workflow passes actionlint and zizmor."
exit 0
