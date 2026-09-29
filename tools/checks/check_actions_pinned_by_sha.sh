#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# tools/checks/check_actions_pinned_by_sha.sh
#
# >>> help
# Is every third-party action a workflow `uses:` pinned by commit SHA?
#
#   tools/checks/check_actions_pinned_by_sha.sh
#   tools/checks/check_actions_pinned_by_sha.sh --root <dir>
#
# Every third-party action a workflow `uses:` is pinned to a full commit
# SHA, with the release it is in a trailing comment:
#
#     uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
#
# WHY. A tag is a movable pointer in someone else's repository. Whoever can
# push to that repository — its maintainers, or anyone who steals one of
# their tokens — can point `@v7` at new code, and every workflow here runs
# it on its next trigger, with this repository's GITHUB_TOKEN and whatever
# secrets the job holds. It has happened: in March 2025 a widely used
# third-party action had every one of its tags rewritten to a commit that
# dumped runner secrets into the logs. A commit SHA cannot be moved; the code it names is the code
# that was reviewed when the pin changed.
#
# The comment is not decoration. Dependabot's github-actions ecosystem
# updates a SHA pin only when it can read the version beside it, and it
# rewrites both together; without the comment the pin freezes silently and
# the repository stops receiving the actions' security fixes. A reader also
# needs it: a bare SHA says nothing about which release is running.
#
# Exempt: a local action or reusable workflow (`./…`), which is this
# repository at the commit being run, and a `docker://` image pinned by
# `@sha256:` digest. A docker image by tag is rejected for the same reason
# a tag is.
#
# What this does NOT prove: that the SHA belongs to the named repository's
# own history rather than a fork's (GitHub serves a fork's commit under the
# parent's path), or that the comment names the release the SHA is in.
# Both need the network; the diff that changes a pin is where they are read.
#
# Options:
#   --root <dir>   check this repository instead of the one containing
#                  this script
#   -h, --help     this text
#
# Exit codes:
#   0  every action is pinned by SHA with its version comment
#   1  at least one is not
#   2  invocation problem
# <<< help

set -uo pipefail

SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
REPO_ROOT="$( cd -- "$SCRIPT_DIR/../.." && pwd )"
me="check_actions_pinned_by_sha"

while [ $# -gt 0 ]; do
    case "$1" in
        -h|--help) sed -n '/^# >>> help$/,/^# <<< help$/p' "$0" | sed -e '1d' -e '$d' -e 's/^# \{0,1\}//'; exit 0 ;;
        --root)    [ $# -ge 2 ] || { echo "$me: --root needs a value" >&2; exit 2; }
                   REPO_ROOT="$2"; shift 2 ;;
        *)         echo "$me: unexpected argument: $1" >&2; exit 2 ;;
    esac
done

shopt -s nullglob globstar
files=("$REPO_ROOT"/.github/workflows/*.y*ml "$REPO_ROOT"/.github/actions/**/action.y*ml)
shopt -u globstar

if (( ${#files[@]} == 0 )); then
    echo "$me: no workflow under $REPO_ROOT/.github/workflows" >&2
    exit 2
fi

bad=0 pinned=0
for f in "${files[@]}"; do
    rel="${f#"$REPO_ROOT"/}"
    # `uses:` as a step key or a list item's first key; a commented-out line
    # starts with `#` and does not match.
    while IFS= read -r line; do
        num="${line%%:*}"
        text="${line#*:}"
        rest="$(sed -E 's/^[[:space:]]*(-[[:space:]]+)?uses:[[:space:]]*//' <<<"$text")"
        ref="$(sed -E 's/[[:space:]]*#.*$//; s/[[:space:]]+$//; s/^["'"'"']//; s/["'"'"']$//' <<<"$rest")"
        comment=""
        [[ "$rest" == *'#'* ]] && comment="$(sed -E 's/^[^#]*#[[:space:]]*//; s/[[:space:]]+$//' <<<"$rest")"

        case "$ref" in
            ./*) continue ;;
            docker://*@sha256:*)
                [[ "${ref##*@sha256:}" =~ ^[0-9a-f]{64}$ ]] && { pinned=$((pinned + 1)); continue; }
                echo "FAIL: $rel:$num '$ref' — a sha256 digest is 64 hex characters"
                bad=$((bad + 1)); continue ;;
            docker://*)
                echo "FAIL: $rel:$num '$ref' — a docker action is pinned by @sha256: digest, not by tag"
                bad=$((bad + 1)); continue ;;
        esac

        if [[ "$ref" != *@* ]]; then
            echo "FAIL: $rel:$num '$ref' names no ref at all — it runs the default branch"
            bad=$((bad + 1)); continue
        fi
        at="${ref##*@}"
        if ! [[ "$at" =~ ^[0-9a-f]{40}$ ]]; then
            echo "FAIL: $rel:$num '$ref' is pinned to '$at', which its owner can move — pin the 40-character commit SHA"
            bad=$((bad + 1)); continue
        fi
        if ! [[ "$comment" =~ ^v?[0-9]+(\.[0-9]+)*([-+][0-9A-Za-z.-]+)?$ ]]; then
            echo "FAIL: $rel:$num '$ref' carries no '# vX.Y.Z' comment — Dependabot cannot update a pin it cannot read the version of"
            bad=$((bad + 1)); continue
        fi
        pinned=$((pinned + 1))
    done < <(grep -nE '^[[:space:]]*(-[[:space:]]+)?uses:[[:space:]]*[^[:space:]]' "$f")
done

if (( bad > 0 )); then
    echo
    echo "A tag or branch is a pointer the action's owner — or whoever holds"
    echo "their token — can move to new code, which then runs here with this"
    echo "repository's token and secrets. Resolve the release to its commit:"
    echo
    echo "    git ls-remote --tags https://github.com/<owner>/<repo> 'v1.2.3*'"
    echo
    echo "(an annotated tag lists its commit on the '^{}' line) and write"
    echo "    uses: <owner>/<repo>@<40-hex sha> # v1.2.3"
    exit 1
fi

echo "$me: OK — $pinned third-party action use(s), every one pinned by SHA with its version."
exit 0
