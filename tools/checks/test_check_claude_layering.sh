#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/test_check_claude_layering.sh
#
# Self-test for check_claude_layering.sh.
#
# The guard reads `cargo metadata`, so these cases drive it through a stub
# `cargo` on PATH that prints a hand-built graph, and a sandbox workspace
# whose Cargo.toml carries the planned_members list. Each offender class
# (crates/transport/*, crates/discovery/*, libp2p) has a case that must
# FAIL, from each bridge package, and each carve-out (a dev-dependency, the
# neutral crates/api/* including discovery-api, a member outside the bridge,
# a directory that only begins with a bridge path) one that must pass — the
# carve-outs are where a guard written too wide would go red on the tree,
# and too narrow would miss the breach next to them.
#
# Exit codes:
#   0  all assertions passed
#   1  one or more failed

set -uo pipefail

SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
UNDER_TEST="$SCRIPT_DIR/check_claude_layering.sh"
[[ -f "$UNDER_TEST" ]] || { echo "test: $UNDER_TEST not found" >&2; exit 1; }

failures=0
SANDBOX="$(mktemp -d)"
trap 'rm -rf "$SANDBOX"' EXIT
WS="$SANDBOX/ws"
mkdir -p "$WS" "$SANDBOX/bin"

pass() { echo "  ✓ $1"; }
fail() { echo "  ✗ $1" >&2; printf '%s\n' "${2:-}" | sed 's/^/      /' >&2
         failures=$((failures + 1)); }

# planned <dir…>: the sandbox manifest's planned_members.
planned() {
    { printf '[workspace]\nmembers = []\n\n[workspace.metadata.interweave]\nplanned_members = [\n'
      for d in "$@"; do printf '  "%s",\n' "$d"; done
      printf ']\n'; } > "$WS/Cargo.toml"
}

# graph <members> <package lines> <edge lines>: the metadata the stub
# prints. A package line is `id name manifest-dir`; an edge line is
# `from to kind` with kind normal, build or dev.
graph() {
    python3 - "$SANDBOX/meta.json" "$WS" "$1" "$2" "$3" <<'PYEOF'
import json, sys
out, ws, members, pkgs, edges = sys.argv[1:6]
packages, nodes = [], {}
for line in filter(None, pkgs.split("\n")):
    pid, name, d = line.split()
    packages.append({"id": pid, "name": name, "manifest_path": f"{ws}/{d}/Cargo.toml"})
    nodes[pid] = {"id": pid, "deps": []}
for line in filter(None, edges.split("\n")):
    a, b, kind = line.split()
    nodes[a]["deps"].append({"pkg": b, "dep_kinds": [{"kind": None if kind == "normal" else kind}]})
json.dump({"workspace_root": ws, "workspace_members": members.split(),
           "packages": packages, "resolve": {"nodes": list(nodes.values())}}, open(out, "w"))
PYEOF
}

cat > "$SANDBOX/bin/cargo" <<EOF
#!/usr/bin/env bash
echo "    Updating crates.io index" >&2
printf '%s\n' "\$*" > "$SANDBOX/cargo-args"
[[ -e "$SANDBOX/cargo-fails" ]] && { echo "error: failed to load manifest" >&2; exit 101; }
cat "$SANDBOX/meta.json"
EOF
chmod +x "$SANDBOX/bin/cargo"

# expect <exit> <name> [<output substring>]
expect() {
    local want="$1" name="$2" says="${3:-}" out got
    out="$(PATH="$SANDBOX/bin:$PATH" bash "$UNDER_TEST" 2>&1)"; got=$?
    if [[ "$got" -ne "$want" ]]; then fail "$name — wanted exit $want, got $got" "$out"; return; fi
    if [[ -n "$says" && "$out" != *"$says"* ]]; then fail "$name — output lacks: $says" "$out"; return; fi
    pass "$name (exit $got)"
}

PKGS='core interweave-claude-channel-core crates/claude/channel-core
app interweave-claude-channel apps/claude-channel
extra interweave-claude-extra crates/claude/extra
near interweave-claude-near crates/claudette
appnear interweave-claude-channel-tools apps/claude-channel-tools
ipcc interweave-ipc-client crates/local/ipc-client
proto interweave-ipc-protocol crates/api/ipc-protocol
api interweave-local-client-api crates/api/local-client-api
tapi interweave-transport-api crates/api/transport-api
dapi interweave-discovery-api crates/api/discovery-api
daemon interweave-transport-daemon apps/transport-daemon
serde serde registry/serde
rt interweave-transport-runtime crates/transport/runtime
comp interweave-transport-composition crates/transport/composition
cache interweave-discovery-cache crates/discovery/cache
lp libp2p registry/libp2p
lpid libp2p-identity third_party/libp2p-identity'
EDGES='ipcc proto normal
proto api normal
api tapi normal
api serde normal'
BOTH="crates/claude/channel-core apps/claude-channel"
BASE="ipcc proto api tapi dapi serde"

echo "test_check_claude_layering"

planned $BOTH
graph "$BASE" "$PKGS" "$EDGES"
expect 0 "both bridge packages planned, not members, pass" "apps/claude-channel is planned, not a member yet"
[[ "$(cat "$SANDBOX/cargo-args")" == *"--all-features"* ]] && pass "  and asks cargo for the all-features graph" \
    || fail "cargo metadata was not asked for --all-features" "$(cat "$SANDBOX/cargo-args")"

# The shape §19 plans: channel-core on the contracts, the app on ipc-client.
graph "$BASE core app" "$PKGS" "$EDGES
core api normal
core tapi normal
app core normal
app ipcc normal
app dapi normal"
expect 0 "channel-core on crates/api/*, the app on ipc-client and discovery-api, pass" "apps/claude-channel reaches no crates/transport/*"
graph "$BASE core" "$PKGS" "$EDGES
core api normal"
expect 0 "channel-core a member, the app still planned, passes" "apps/claude-channel is planned"

# The offenders: transport, discovery, libp2p — direct, transitive, build.
graph "$BASE core app" "$PKGS" "$EDGES
core rt normal"
expect 1 "channel-core depending on crates/transport/* fails, with the path" "crates/claude/channel-core depends on a crate under crates/transport/: interweave-claude-channel-core -> interweave-transport-runtime"
graph "$BASE core app" "$PKGS" "$EDGES
app ipcc normal
ipcc comp normal"
expect 1 "the app reaching crates/transport/* through ipc-client fails" "apps/claude-channel depends on a crate under crates/transport/: interweave-claude-channel -> interweave-ipc-client -> interweave-transport-composition"
graph "$BASE core app" "$PKGS" "$EDGES
core cache normal"
expect 1 "channel-core depending on crates/discovery/* fails" "crates/claude/channel-core depends on a crate under crates/discovery/"
graph "$BASE core app" "$PKGS" "$EDGES
app ipcc normal
tapi lpid normal"
expect 1 "a libp2p-* crate four hops down fails, vendored or not" "interweave-claude-channel -> interweave-ipc-client -> interweave-ipc-protocol -> interweave-local-client-api -> interweave-transport-api -> libp2p-identity"
graph "$BASE core app" "$PKGS" "$EDGES
app lp build"
expect 1 "a build-dependency counts" "apps/claude-channel depends on a libp2p crate: interweave-claude-channel -> libp2p"
graph "$BASE core app" "$PKGS" "$EDGES
core rt normal
app lp normal"
expect 1 "both packages' breaches are reported, not the first alone (the app sorts first)" "crates/claude/channel-core depends on a crate under crates/transport/"
graph "$BASE core app" "$PKGS" "$EDGES
core rt normal
core cache normal"
expect 1 "two offenders under one package are both reported" "crates/claude/channel-core depends on a crate under crates/discovery/"
graph "$BASE core app" "$PKGS" "$EDGES
core rt dev
app comp dev
app lp dev"
expect 0 "dev-dependencies are not followed"

# Placement by directory: a new crates/claude/* member is walked unnamed.
graph "$BASE core app extra" "$PKGS" "$EDGES
extra cache normal"
expect 1 "another crates/claude/* member is walked too" "crates/claude/extra depends on a crate under crates/discovery/"

# Scope: the guard is the bridge's, not the workspace's.
graph "$BASE core app daemon near appnear" "$PKGS" "$EDGES
daemon rt normal
daemon lp normal
near rt normal
appnear lp normal"
expect 0 "the daemon, crates/claudette and apps/claude-channel-tools are not the bridge"

# Absent packages: planned is a skip, anything else a failure to check.
planned apps/claude-channel
graph "$BASE" "$PKGS" "$EDGES"
expect 2 "channel-core neither member nor planned is exit 2, by name" "crates/claude/channel-core is neither a workspace member nor in planned_members"
planned crates/claude/channel-core
graph "$BASE" "$PKGS" "$EDGES"
expect 2 "the app neither member nor planned is exit 2, by name" "apps/claude-channel is neither a workspace member nor in planned_members"
planned
graph "$BASE app" "$PKGS" "$EDGES"
expect 2 "a member dropped from the workspace and the plan is exit 2, not a pass" "crates/claude/channel-core is neither"
printf '[workspace]\nmembers = []\n' > "$WS/Cargo.toml"
graph "$BASE" "$PKGS" "$EDGES"
expect 2 "no planned_members in the manifest is exit 2" "cannot read [workspace.metadata.interweave].planned_members"
planned $BOTH

python3 -c "import json; json.dump({'workspace_root': '$WS', 'workspace_members': [], 'packages': [], 'resolve': None}, open('$SANDBOX/meta.json', 'w'))"
expect 2 "metadata without a resolved graph is exit 2" "no resolved dependency graph"
python3 -c "import json; json.dump({'workspace_root': '$WS', 'workspace_members': ['core'], 'packages': [{'id': 'core', 'name': 'interweave-claude-channel-core', 'manifest_path': '$WS/crates/claude/channel-core/Cargo.toml'}], 'resolve': {'nodes': [{'id': 'other', 'deps': []}]}}, open('$SANDBOX/meta.json', 'w'))"
expect 2 "a bridge package with no node in the graph is exit 2" "has no node in the resolved graph"
python3 -c "import json; json.dump({'workspace_root': '$WS', 'workspace_members': ['core'], 'packages': [{'id': 'core', 'name': 'interweave-claude-channel-core', 'manifest_path': '$WS/crates/claude/channel-core/Cargo.toml'}, {'id': 'mid', 'name': 'interweave-mid', 'manifest_path': '$WS/crates/api/mid/Cargo.toml'}], 'resolve': {'nodes': [{'id': 'core', 'deps': [{'pkg': 'mid', 'dep_kinds': [{'kind': None}]}]}]}}, open('$SANDBOX/meta.json', 'w'))"
expect 2 "a crate it reaches with no node in the graph is exit 2, not a pass" "interweave-mid has no node in the resolved graph"
python3 -c "import json; json.dump({'workspace_root': '$WS', 'workspace_members': ['core'], 'packages': [{'id': 'core', 'name': 'interweave-claude-channel-core', 'manifest_path': '$WS/crates/claude/channel-core/Cargo.toml'}], 'resolve': {'nodes': [{'id': 'core', 'deps': [{'pkg': 'zz', 'dep_kinds': [{'kind': None}]}]}]}}, open('$SANDBOX/meta.json', 'w'))"
expect 2 "an unexpected error in the walk is exit 2, not a breach" "could not walk cargo metadata"
# An error while loading, before the walk: a package with no manifest path.
python3 -c "import json; json.dump({'workspace_root': '$WS', 'workspace_members': ['core'], 'packages': [{'id': 'core', 'name': 'interweave-claude-channel-core'}], 'resolve': {'nodes': [{'id': 'core', 'deps': []}]}}, open('$SANDBOX/meta.json', 'w'))"
expect 2 "an unexpected error while loading the graph is exit 2, not a breach" "could not walk cargo metadata"
printf 'not json' > "$SANDBOX/meta.json"
expect 2 "output that is not JSON is exit 2" "not JSON"
touch "$SANDBOX/cargo-fails"
expect 2 "cargo metadata failing is exit 2, with cargo's own error" "failed to load manifest"
rm -f "$SANDBOX/cargo-fails"

help_out="$(bash "$UNDER_TEST" --help 2>/dev/null)"
[[ "$help_out" == *"Plan §19's P2"* ]] && pass "--help prints the help block" || fail "--help should print the help block" "$help_out"

echo
if (( failures > 0 )); then
    echo "test_check_claude_layering: $failures failure(s)" >&2
    exit 1
fi
echo "test_check_claude_layering: OK — all assertions passed."
