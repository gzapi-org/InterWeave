#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# tools/checks/check_independent_codecs.sh
#
# >>> help
# Do the independent codecs stay independent of this repository's code?
#
#   tools/checks/check_independent_codecs.sh
#
# Stage 17's independent codecs (plan §20 (c); testing.md §Compatibility
# fixtures, A 2026-10-08) are written from the contract text so that a
# disagreement with production is a contract finding. That holds only
# while interweave-independent-codecs shares no code with production: a
# codec that reached a production crate, even through a test helper,
# would agree with it for free.
#
# THE RULE: no package in the crate's dependency graph may be a PATH
# package — a workspace member, tests/support, or a vendored tree under
# third_party/ alike — only registry crates. A path package is this
# repository's code whatever its name, so the rule needs no list of
# production crates to go stale.
#
# WHAT IS FOLLOWED: from the codecs crate itself, every dependency kind —
# normal, build AND dev. That is the difference from the layering checks,
# which follow no dev-dependency: a test of the codecs that called a
# production decoder would compare production with itself. Below the first
# hop, normal and build only, since a dependency's own dev-dependencies
# are never compiled for it. --all-features, so an optional dependency is
# in the graph too.
#
# Exit codes:
#   0  the codecs reach no path package
#   1  they do; the path from the crate to the offender is printed
#   2  cargo metadata failed or returned no resolved graph, or the crate
#      is not a workspace member (the check would cover nothing)
# <<< help

set -uo pipefail

ROOT="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )/../.." && pwd )"
cd "$ROOT" || exit 2

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
    sed -n '/^# >>> help$/,/^# <<< help$/p' "$0" | sed '1d;$d;s/^# \{0,1\}//'
    exit 0
fi

# stderr apart from the JSON: a successful run may print an index update,
# and one such line in the document would make the walk fail. Shown only
# when cargo fails.
err="$(mktemp)"; trap 'rm -f "$err"' EXIT
meta="$(cargo metadata --format-version 1 --locked --all-features 2>"$err")" || {
    echo "check_independent_codecs: cargo metadata failed:" >&2
    tail -5 "$err" >&2
    exit 2
}

read -r -d '' walk <<'PYEOF'
import json, sys

CRATE = "interweave-independent-codecs"

try:
    meta = json.load(sys.stdin)
except ValueError as e:
    print(f"check_independent_codecs: cargo metadata's output is not JSON ({e})", file=sys.stderr)
    sys.exit(2)
nodes_list = (meta.get("resolve") or {}).get("nodes")
if not nodes_list:
    print("check_independent_codecs: cargo metadata has no resolved dependency graph", file=sys.stderr)
    sys.exit(2)
packages = {p["id"]: p for p in meta["packages"]}
members = set(meta.get("workspace_members", []))
nodes = {n["id"]: n for n in nodes_list}

def deps(node, first_hop):
    for d in node.get("deps", []):
        kinds = [k.get("kind") for k in d.get("dep_kinds", [])]
        if first_hop or any(k in (None, "build") for k in kinds):
            yield d["pkg"]

def walk():
    ids = [i for i, p in packages.items() if p["name"] == CRATE and i in members]
    if not ids:
        print(f"check_independent_codecs: {CRATE} is not a workspace member — the check would cover nothing", file=sys.stderr)
        sys.exit(2)
    start = ids[0]
    if start not in nodes:
        print(f"check_independent_codecs: {CRATE} has no node in the resolved graph", file=sys.stderr)
        sys.exit(2)
    parent = {start: None}
    queue = [start]
    bad = 0
    while queue:
        cur = queue.pop(0)
        for dep in deps(nodes[cur], cur == start):
            if dep in parent:
                continue
            parent[dep] = cur
            if packages[dep].get("source") is None:
                chain, at = [], dep
                while at is not None:
                    chain.append(packages[at]["name"])
                    at = parent[at]
                print(f"FAIL: {CRATE} reaches a path package: {' -> '.join(reversed(chain))}")
                bad += 1
                continue
            queue.append(dep)
    if bad:
        print()
        print("The independent codecs are written from the contract text alone")
        print("(testing.md §Compatibility fixtures). A comparison with production")
        print("belongs in tests/interoperability, which may depend on both.")
        sys.exit(1)
    print(f"check_independent_codecs: {CRATE} reaches no path package ({len(parent) - 1} dependencies walked, dev included)")

try:
    walk()
except Exception as e:
    print(f"check_independent_codecs: could not walk cargo metadata ({type(e).__name__}: {e})", file=sys.stderr)
    sys.exit(2)
PYEOF
python3 -c "$walk" <<<"$meta"
