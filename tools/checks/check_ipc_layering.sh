#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# tools/checks/check_ipc_layering.sh
#
# >>> help
# Do the IPC server and client stay off the transport runtime?
#
#   tools/checks/check_ipc_layering.sh
#
# Plan §16's D2. interweave-ipc-server (and interweave-ipc-client, once
# it is a workspace member) may depend on nothing under crates/transport/*
# and on no libp2p crate, directly or through anything they depend on. A
# server that cannot name the runtime cannot hold a second lease table or
# queue of its own, which is half of Stage 13's exit gate; the composition
# root (apps/transport-daemon) hands it a binding instead.
#
# WHAT IS FOLLOWED: the normal and build dependencies cargo resolves, to
# any depth. A dev-dependency is not followed: a test may reach for the
# composition crate, and it is not compiled into the library. A crate is
# "under crates/transport/" by where its Cargo.toml sits, so a new crate
# there is caught without this file naming it; a libp2p crate by its
# package name (`libp2p` or `libp2p-*`), whether from crates.io or
# vendored under third_party/.
#
# A CRATE THAT IS NOT A WORKSPACE MEMBER is reported, not failed, for the
# client only: it lands in B3, and until then there is nothing to check.
# The server must exist; a server renamed or dropped from the workspace
# would otherwise make this pass by checking nothing.
#
# Exit codes:
#   0  neither crate reaches crates/transport/* or libp2p
#   1  one does; the path from the crate to the offender is printed
#   2  cargo metadata failed, or the server is not a workspace member
# <<< help

set -uo pipefail

ROOT="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )/../.." && pwd )"
cd "$ROOT" || exit 2

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
    sed -n '/^# >>> help$/,/^# <<< help$/p' "$0" | sed '1d;$d;s/^# \{0,1\}//'
    exit 0
fi

meta="$(cargo metadata --format-version 1 --locked 2>&1)" || {
    echo "check_ipc_layering: cargo metadata failed:" >&2
    printf '%s\n' "$meta" | tail -5 >&2
    exit 2
}

# The program is read into a variable and passed with -c, so stdin is
# free for the metadata (a heredoc and a here-string on one command
# would both claim it, and the later one wins).
read -r -d '' walk <<'PYEOF'
import json, os, sys

root = os.path.realpath(sys.argv[1])
meta = json.load(sys.stdin)
ws_root = os.path.realpath(meta.get("workspace_root", root))
packages = {p["id"]: p for p in meta["packages"]}
members = set(meta.get("workspace_members", []))
nodes = {n["id"]: n for n in (meta.get("resolve") or {}).get("nodes", [])}
transport = os.path.join(ws_root, "crates", "transport") + os.sep

# (crate, required): the server must exist, the client lands in B3.
GUARDED = [("interweave-ipc-server", True), ("interweave-ipc-client", False)]

def offence(pkg):
    name = pkg["name"]
    if name == "libp2p" or name.startswith("libp2p-"):
        return "a libp2p crate"
    manifest = os.path.realpath(pkg["manifest_path"])
    if manifest.startswith(transport):
        return "a crate under crates/transport/"
    return None

def runtime_deps(node):
    # A dependency counts when any of its kinds is normal (null) or build.
    for d in node.get("deps", []):
        if any(k.get("kind") in (None, "build") for k in d.get("dep_kinds", [])):
            yield d["pkg"]

bad = 0
for crate, required in GUARDED:
    ids = [i for i, p in packages.items() if p["name"] == crate and i in members]
    if not ids:
        if required:
            print(f"check_ipc_layering: {crate} is not a workspace member — the guard would check nothing", file=sys.stderr)
            sys.exit(2)
        print(f"check_ipc_layering: {crate} is not a workspace member yet — nothing to check for it")
        continue
    start = ids[0]
    # Breadth-first, keeping each crate's parent, so a finding prints the
    # shortest path from the guarded crate to it.
    parent = {start: None}
    queue = [start]
    while queue:
        cur = queue.pop(0)
        for dep in runtime_deps(nodes.get(cur, {})):
            if dep in parent:
                continue
            parent[dep] = cur
            why = offence(packages[dep])
            if why:
                chain, at = [], dep
                while at is not None:
                    chain.append(packages[at]["name"])
                    at = parent[at]
                print(f"FAIL: {crate} depends on {why}: {' -> '.join(reversed(chain))}")
                bad += 1
                continue  # the offender itself is enough; do not walk into it
            queue.append(dep)
    if not bad:
        print(f"check_ipc_layering: {crate} reaches no crates/transport/* crate and no libp2p crate ({len(parent) - 1} runtime dependencies walked)")

if bad:
    print()
    print("The IPC server and client take the transport through the neutral")
    print("binding traits (crates/api/*); the composition root wires the runtime")
    print("in. Move the dependency to apps/transport-daemon, or behind a trait.")
    sys.exit(1)
PYEOF
python3 -c "$walk" "$ROOT" <<<"$meta"
