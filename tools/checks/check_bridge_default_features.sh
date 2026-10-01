#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# tools/checks/check_bridge_default_features.sh
#
# >>> help
# Does the Claude bridge build without a CommonMark parser by default?
#
#   tools/checks/check_bridge_default_features.sh
#
# ADR-0050 (A 2026-10-01) and plan §17 (3): the bridge does no Markdown
# parsing, so chat-protocol's parser sits behind a feature that is off by
# default. This check holds every workspace member under crates/claude/*
# and apps/claude-channel (the bridge's composition root, whose own
# manifest could enable the feature) to that: `cargo tree -e normal` of
# each, under default features, names no CommonMark parser
# (pulldown-cmark, comrak, markdown).
#
# A DEPENDENCY-GRAPH CLAIM, NOT AN ARTIFACT CLAIM. A workspace-wide build
# unifies features, so a binary built with --workspace may well link the
# parser; what is held is the graph each bridge package resolves on its
# own (`cargo tree -p`). cargo metadata cannot say this: its resolve is
# one unified graph for the whole workspace.
#
# POSITIVE CONTROL: each package's tree must name that package as its
# root, or this is exit 2 — a tree that came back empty, or for another
# package, would otherwise pass having checked nothing. Until the bridge
# packages exist, crates/claude/channel-core and apps/claude-channel are
# reported as planned and skipped while planned_members names them; absent
# from both, exit 2.
#
# Exit codes:
#   0  no bridge package's default-feature graph names a parser
#   1  one does; the package and the parser are printed
#   2  cargo failed, a tree did not name its own package, planned_members
#      could not be read, or a bridge package is neither member nor planned
# <<< help

set -uo pipefail

ROOT="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )/../.." && pwd )"
cd "$ROOT" || exit 2
me="check_bridge_default_features"

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
    sed -n '/^# >>> help$/,/^# <<< help$/p' "$0" | sed '1d;$d;s/^# \{0,1\}//'
    exit 0
fi

PARSERS=(pulldown-cmark comrak markdown)
REQUIRED=(crates/claude/channel-core apps/claude-channel)

err="$(mktemp)"; trap 'rm -f "$err"' EXIT
meta="$(cargo metadata --format-version 1 --locked --no-deps 2>"$err")" || {
    echo "$me: cargo metadata failed:" >&2; tail -5 "$err" >&2; exit 2
}

# "<dir> <package>" for every member under crates/claude/ or at
# apps/claude-channel, then "planned <dir>" for each planned member.
listing="$(python3 -c '
import json, os, sys, tomllib
meta = json.load(sys.stdin)
ws = os.path.realpath(meta["workspace_root"])
pk = {p["id"]: p for p in meta["packages"]}
for i in meta["workspace_members"]:
    d = os.path.relpath(os.path.dirname(os.path.realpath(pk[i]["manifest_path"])), ws)
    if d.startswith("crates/claude/") or d == "apps/claude-channel":
        print(d, pk[i]["name"])
with open(os.path.join(ws, "Cargo.toml"), "rb") as f:
    for d in tomllib.load(f)["workspace"]["metadata"]["interweave"]["planned_members"]:
        print("planned", d)
' <<<"$meta" 2>"$err")" || {
    echo "$me: cannot read the workspace members or planned_members:" >&2; tail -3 "$err" >&2; exit 2
}

declare -A member_pkg=() planned=()
while read -r a b; do
    [[ -z "$a" ]] && continue
    if [[ "$a" == "planned" ]]; then planned["$b"]=1; else member_pkg["$a"]="$b"; fi
done <<<"$listing"

for d in "${REQUIRED[@]}"; do
    [[ -n "${member_pkg[$d]:-}" ]] && continue
    if [[ -n "${planned[$d]:-}" ]]; then
        echo "$me: $d is planned, not a member yet — nothing to check for it"
    else
        echo "$me: $d is neither a workspace member nor in planned_members — the check would cover nothing" >&2
        exit 2
    fi
done

bad=0
for d in $(printf '%s\n' "${!member_pkg[@]}" | sort); do
    pkg="${member_pkg[$d]}"
    tree="$(cargo tree --locked -e normal -p "$pkg" --prefix none --format '{p}' 2>"$err")" || {
        echo "$me: cargo tree -p $pkg failed:" >&2; tail -5 "$err" >&2; exit 2
    }
    # The positive control: the first line is the package itself.
    root_name="$(head -1 <<<"$tree" | cut -d' ' -f1)"
    if [[ "$root_name" != "$pkg" ]]; then
        echo "$me: cargo tree -p $pkg did not name $pkg as its root (got '${root_name:-nothing}') — not checked" >&2
        exit 2
    fi
    found=""
    for p in "${PARSERS[@]}"; do
        if cut -d' ' -f1 <<<"$tree" | grep -qxF -- "$p"; then found+=" $p"; fi
    done
    if [[ -n "$found" ]]; then
        echo "FAIL: $d ($pkg) names a CommonMark parser under default features:$found"
        bad=$((bad + 1))
    else
        echo "$me: $d ($pkg) names no CommonMark parser under default features ($(wc -l <<<"$tree") packages in its tree)"
    fi
done

if (( bad )); then
    echo
    echo "The bridge does no Markdown parsing (ADR-0050): keep chat-protocol's"
    echo "parser feature off by default, and enable it in no bridge manifest."
    echo "\`cargo tree -e normal -p <package> -i pulldown-cmark\` shows which edge pulls it."
    exit 1
fi
exit 0
