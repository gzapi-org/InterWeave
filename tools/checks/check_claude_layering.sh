#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# tools/checks/check_claude_layering.sh
#
# >>> help
# Does the Claude bridge stay off the transport and discovery internals?
#
#   tools/checks/check_claude_layering.sh
#
# Plan §19's P2 and its Rule: the bridge consumes only transport-neutral
# local-client/IPC models. Every workspace member under crates/claude/ and
# apps/claude-channel (the bridge's composition root) reaches nothing under
# crates/transport/, nothing under crates/discovery/ and no libp2p crate.
# It reaches the daemon through ipc-client and the neutral contracts under
# crates/api/; §19 step 2 moves the reply-token table out of
# crates/transport/runtime into channel-core for this reason.
# check_human_layering.sh is the precedent; check_ipc_layering.sh holds
# ipc-client, which the bridge depends on, to the same transport rule.
#
# WHAT IS FOLLOWED: the normal and build dependencies cargo resolves with
# --all-features, to any depth (a dependency optional behind a feature is
# linked by a build that enables it). A dev-dependency is not followed: a
# test may reach for the daemon's composition, and it is not compiled into
# the bridge. A crate is placed by where its Cargo.toml sits, so a new
# crate under crates/claude/ is walked, and a new one under
# crates/transport/ or crates/discovery/ refused, without this file naming
# it; libp2p by package name (`libp2p`, `libp2p-*`), whether from crates.io
# or vendored under third_party/.
#
# THE TWO PACKAGES §19 ACTIVATES (crates/claude/channel-core and
# apps/claude-channel) THAT ARE NOT WORKSPACE MEMBERS pass only while
# [workspace.metadata.interweave].planned_members names them, and are
# reported as planned. Absent from both, the guard would pass having
# checked nothing, so that is exit 2.
#
# Exit codes:
#   0  no bridge package reaches crates/transport/*, crates/discovery/* or
#      libp2p
#   1  one does; the path from the package to the offender is printed
#   2  a failure to check: cargo metadata failed or returned no resolved
#      graph, a bridge package or a crate it reaches has no node in it, planned_members could not
#      be read, a required package is neither a member nor planned, or the
#      load or the walk raised an error it did not expect
# <<< help

set -uo pipefail

ROOT="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )/../.." && pwd )"
cd "$ROOT" || exit 2

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
    sed -n '/^# >>> help$/,/^# <<< help$/p' "$0" | sed '1d;$d;s/^# \{0,1\}//'
    exit 0
fi

# stderr apart from the JSON, as check_ipc_layering.sh: a successful run
# may still print an index update or a manifest warning, and one such line
# in the document would read as a breach. Shown only when cargo fails.
err="$(mktemp)"; trap 'rm -f "$err"' EXIT
meta="$(cargo metadata --format-version 1 --locked --all-features 2>"$err")" || {
    echo "check_claude_layering: cargo metadata failed:" >&2
    tail -5 "$err" >&2
    exit 2
}

read -r -d '' walk <<'PYEOF'
import os, sys

me = "check_claude_layering"

def load():
    """The graph and the roster, as module globals. Inside the try below, so
    an error here (a Python without tomllib, a package with no manifest
    path) is a failure to check, exit 2 — never exit 1, which is a breach."""
    global ws_root, planned, packages, nodes, member_at
    import json, tomllib
    root = os.path.realpath(sys.argv[1])
    try:
        meta = json.load(sys.stdin)
    except ValueError as e:
        print(f"{me}: cargo metadata's output is not JSON ({e})", file=sys.stderr)
        sys.exit(2)
    if not (meta.get("resolve") or {}).get("nodes"):
        print(f"{me}: cargo metadata has no resolved dependency graph", file=sys.stderr)
        sys.exit(2)
    ws_root = os.path.realpath(meta.get("workspace_root", root))
    try:
        with open(os.path.join(ws_root, "Cargo.toml"), "rb") as f:
            planned = set(tomllib.load(f)["workspace"]["metadata"]["interweave"]["planned_members"])
    except (OSError, KeyError, TypeError, tomllib.TOMLDecodeError) as e:
        print(f"{me}: cannot read [workspace.metadata.interweave].planned_members ({type(e).__name__}: {e})", file=sys.stderr)
        sys.exit(2)
    packages = {p["id"]: p for p in meta["packages"]}
    members = [i for i in meta.get("workspace_members", []) if i in packages]
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    member_at = {rel_dir(packages[i]): i for i in members}

def rel_dir(pkg):
    return os.path.relpath(os.path.dirname(os.path.realpath(pkg["manifest_path"])), ws_root)

REQUIRED = ["crates/claude/channel-core", "apps/claude-channel"]
CLAUDE = "crates/claude" + os.sep
APP = "apps/claude-channel"

def is_bridge(d):
    return d == APP or (d + os.sep).startswith(CLAUDE)

def offence(pkg):
    name = pkg["name"]
    if name == "libp2p" or name.startswith("libp2p-"):
        return "a libp2p crate"
    d = rel_dir(pkg) + os.sep
    if d.startswith("crates/transport" + os.sep):
        return "a crate under crates/transport/"
    if d.startswith("crates/discovery" + os.sep):
        return "a crate under crates/discovery/"
    return None

def runtime_deps(node):
    # A dependency counts when any of its kinds is normal (null) or build.
    for d in node.get("deps", []):
        if any(k.get("kind") in (None, "build") for k in d.get("dep_kinds", [])):
            yield d["pkg"]

def walk(start):
    """Breadth-first from start; each offender printed once, by its shortest path."""
    parent, queue, found = {start: None}, [start], 0
    while queue:
        cur = queue.pop(0)
        if cur not in nodes:
            # A crate with no node would end the walk below it unseen and
            # read as a pass.
            print(f"{me}: {packages[cur]['name']} has no node in the resolved graph", file=sys.stderr)
            sys.exit(2)
        for dep in runtime_deps(nodes[cur]):
            if dep in parent:
                continue
            parent[dep] = cur
            why = offence(packages[dep])
            if why:
                chain, at = [], dep
                while at is not None:
                    chain.append(packages[at]["name"])
                    at = parent[at]
                print(f"FAIL: {rel_dir(packages[start])} depends on {why}: {' -> '.join(reversed(chain))}")
                found += 1
                continue  # the offender is enough; do not walk into it
            queue.append(dep)
    return len(parent) - 1, found

def main():
    for d in REQUIRED:
        if d in member_at:
            continue
        if d in planned:
            print(f"{me}: {d} is planned, not a member yet — nothing to check for it")
            continue
        print(f"{me}: {d} is neither a workspace member nor in planned_members — the guard would check nothing", file=sys.stderr)
        sys.exit(2)
    bad = 0
    for d, start in sorted(member_at.items()):
        if not is_bridge(d):
            continue
        n, found = walk(start)
        bad += found
        if not found:
            print(f"{me}: {d} reaches no crates/transport/*, crates/discovery/* or libp2p crate ({n} runtime dependencies walked)")
    if bad:
        print()
        print("The Claude bridge consumes only transport-neutral local-client/IPC models:")
        print("it reaches the daemon through ipc-client and the contracts under crates/api/.")
        print("Move the dependency behind a neutral contract, or into the daemon.")
        sys.exit(1)

# Exit 1 means a breach and nothing else: an error the load or the walk did
# not expect is a failure to check, exit 2. SystemExit is not an Exception.
try:
    load()
    main()
except Exception as e:
    print(f"{me}: could not walk cargo metadata ({type(e).__name__}: {e})", file=sys.stderr)
    sys.exit(2)
PYEOF
python3 -c "$walk" "$ROOT" <<<"$meta"
