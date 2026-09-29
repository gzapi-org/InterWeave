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
# dumped runner secrets into the logs. A commit SHA cannot be moved; the
# code it names is the code that was reviewed when the pin changed.
#
# The version comment is for the reader: a bare SHA says nothing about
# which release runs, and a pin's diff can only be checked against the
# release it claims. It must sit on the `uses:` line itself, because that
# is the one place Dependabot rewrites it together with the SHA; anywhere
# else it goes stale on the first update. (Dependabot finds the version
# from the upstream tag at the pinned commit, not from the comment.)
#
# `uses` is found as a block key (`uses:`, `- uses:`, quoted, or with a
# space before the colon) and as a key of a flow mapping (`- {uses: …}`),
# all of which GitHub accepts. A value that is not on the key's own line
# fails: it cannot carry the comment.
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

# The key, optionally quoted, optionally followed by a space before the
# colon; the last group is everything after the colon. flow_key finds it
# after the `{` or `,` of a flow mapping.
block_key='^[[:space:]]*(-[[:space:]]+)?["'"'"']?uses["'"'"']?[[:space:]]*:[[:space:]]*(.*)$'
flow_key='[{,][[:space:]]*["'"'"']?uses["'"'"']?[[:space:]]*:[[:space:]]*(.*)$'
bad=0 pinned=0
for f in "${files[@]}"; do
    rel="${f#"$REPO_ROOT"/}"
    num=0
    while IFS= read -r text || [[ -n "$text" ]]; do
        num=$((num + 1))
        # Both keys are anchored to the start of the line or of a flow
        # mapping, so a commented-out line (`# - uses: …`) matches neither.
        if [[ "$text" =~ $block_key ]]; then
            rest="${BASH_REMATCH[2]}"
        elif [[ "$text" =~ ^[[:space:]]*(-[[:space:]]*)?\{ && "$text" =~ $flow_key ]]; then
            rest="${BASH_REMATCH[1]}"
        else
            continue
        fi
        # The value ends at whitespace, a comma or a closing brace; quotes
        # around it are YAML's, not the ref's.
        ref="${rest%%[[:space:],\}#]*}"
        ref="${ref#[\"\']}"; ref="${ref%[\"\']}"
        comment=""
        [[ "$rest" == *'#'* ]] && comment="$(sed -E 's/^[^#]*#[[:space:]]*//; s/[[:space:]]+$//' <<<"$rest")"
        if [[ -z "$ref" ]]; then
            echo "FAIL: $rel:$num a uses: value not on its key's line — it cannot carry the version comment; write it on one line"
            bad=$((bad + 1)); continue
        fi

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
            echo "FAIL: $rel:$num '$ref' carries no '# vX.Y.Z' comment on its line — nothing says which release runs, and Dependabot rewrites only a comment there"
            bad=$((bad + 1)); continue
        fi
        pinned=$((pinned + 1))
    done < "$f"
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
