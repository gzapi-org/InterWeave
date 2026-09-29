#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/test_check_ipc_layering.sh
#
# Self-test for check_ipc_layering.sh.
#
# The guard reads `cargo metadata`, so these cases drive it through a stub
# `cargo` on PATH that prints a hand-built graph: the assertion under test
# is how the guard WALKS the graph, and a real resolution would take a
# network and minutes to say the same thing. The case that matters most is
# the TRANSITIVE one — a crate that pulls the runtime in two hops is the
# same breach as a direct dependency, and the easiest to miss.
#
# Exit codes:
#   0  all assertions passed
#   1  one or more failed

set -uo pipefail

SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
UNDER_TEST="$SCRIPT_DIR/check_ipc_layering.sh"
[[ -f "$UNDER_TEST" ]] || { echo "test: $UNDER_TEST not found" >&2; exit 1; }

failures=0
SANDBOX="$(mktemp -d)"
trap 'rm -rf "$SANDBOX"' EXIT

pass() { echo "  ✓ $1"; }
fail() { echo "  ✗ $1" >&2; printf '%s\n' "${2:-}" | sed 's/^/      /' >&2
         failures=$((failures + 1)); }

# graph <members> <package lines> <edge lines>: writes the metadata the stub
# prints. A package line is `id name manifest-dir`; an edge line is
# `from to kind` with kind normal, build or dev.
graph() {
    python3 - "$SANDBOX/meta.json" "$1" "$2" "$3" <<'PYEOF'
import json, sys
out, members, pkgs, edges = sys.argv[1:5]
packages, nodes = [], {}
for line in filter(None, pkgs.split("\n")):
    pid, name, d = line.split()
    packages.append({"id": pid, "name": name, "manifest_path": f"/ws/{d}/Cargo.toml"})
    nodes[pid] = {"id": pid, "deps": []}
for line in filter(None, edges.split("\n")):
    a, b, kind = line.split()
    nodes[a]["deps"].append({"pkg": b, "dep_kinds": [{"kind": None if kind == "normal" else kind}]})
json.dump({"workspace_root": "/ws", "workspace_members": members.split(),
           "packages": packages, "resolve": {"nodes": list(nodes.values())}}, open(out, "w"))
PYEOF
}

mkdir -p "$SANDBOX/bin"
# The stub always writes progress to stderr, as a cold cargo does on a
# successful run; the guard must keep it out of the JSON.
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

BASE_PKGS='srv interweave-ipc-server crates/local/ipc-server
api interweave-transport-api crates/api/transport-api
proto interweave-ipc-protocol crates/api/ipc-protocol
tokio tokio registry/tokio
rt interweave-transport-runtime crates/transport/runtime
comp interweave-transport-composition crates/transport/composition
p2p libp2p-identity registry/libp2p-identity
lp libp2p registry/libp2p'
BASE_EDGES='srv api normal
srv proto normal
srv tokio normal'

echo "test_check_ipc_layering"

graph "srv api proto rt comp" "$BASE_PKGS" "$BASE_EDGES"
expect 0 "the server on the neutral API alone passes" "reaches no crates/transport/*"
# The graph must be the all-features resolve, or an optional dependency
# behind a non-default feature is not in it.
[[ "$(cat "$SANDBOX/cargo-args")" == *"--all-features"* ]] && pass "  and asks cargo for the all-features graph" \
    || fail "cargo metadata was not asked for --all-features" "$(cat "$SANDBOX/cargo-args")"

graph "srv api proto rt comp" "$BASE_PKGS" "$BASE_EDGES
srv rt normal"
expect 1 "a direct dependency on a crate under crates/transport/ fails" "interweave-ipc-server -> interweave-transport-runtime"

# Two hops: the server's own manifest is clean, the breach is below it.
graph "srv api proto rt comp" "$BASE_PKGS" "$BASE_EDGES
api comp normal"
expect 1 "a transport crate reached through another dependency fails, with the path" "interweave-ipc-server -> interweave-transport-api -> interweave-transport-composition"

graph "srv api proto rt comp" "$BASE_PKGS" "$BASE_EDGES
proto p2p normal"
expect 1 "a libp2p-* crate reached transitively fails" "a libp2p crate"

graph "srv api proto rt comp" "$BASE_PKGS" "$BASE_EDGES
srv lp normal"
expect 1 "the libp2p crate itself fails" "-> libp2p"

graph "srv api proto rt comp" "$BASE_PKGS" "$BASE_EDGES
srv p2p build"
expect 1 "a build-dependency counts: it is compiled into the build" "libp2p-identity"

# A test may reach for the composition crate; it is not in the library.
graph "srv api proto rt comp" "$BASE_PKGS" "$BASE_EDGES
srv comp dev
srv p2p dev"
expect 0 "dev-dependencies are not followed"

graph "srv api proto rt comp" "$BASE_PKGS" "$BASE_EDGES
tokio rt dev"
expect 0 "a dependency's own dev-dependency is not followed either"

# A package NAMED like a transport crate but living elsewhere is fine; the
# rule is where its manifest sits.
graph "srv api proto" "$BASE_PKGS
fake interweave-transport-thing crates/api/fake" "$BASE_EDGES
srv fake normal"
expect 0 "a crate is judged by where its manifest sits, not by its name"

graph "api proto" "$BASE_PKGS" "$BASE_EDGES"
expect 2 "no server in the workspace is a failure to check, not a pass" "not a workspace member"

graph "srv api proto cli" "$BASE_PKGS
cli interweave-ipc-client crates/local/ipc-client" "$BASE_EDGES
cli rt normal"
expect 1 "a member client that reaches the runtime fails" "interweave-ipc-client -> interweave-transport-runtime"

graph "srv api proto" "$BASE_PKGS" "$BASE_EDGES"
expect 0 "an absent client is reported, not failed" "interweave-ipc-client is not a workspace member yet"

# Two offenders in one chain: the walk stops at the first, so the path is
# reported once, through the transport crate, not again through libp2p.
graph "srv api proto rt comp" "$BASE_PKGS" "$BASE_EDGES
srv rt normal
rt p2p normal"
out="$(PATH="$SANDBOX/bin:$PATH" bash "$UNDER_TEST" 2>&1)"
[[ "$(grep -c '^FAIL' <<<"$out")" -eq 1 && "$out" != *"libp2p-identity"* ]] \
    && pass "a chain through one offender is reported once, at the first" \
    || fail "the walk went past the first offender" "$out"

python3 -c "import json; json.dump({'workspace_root': '/ws', 'workspace_members': ['srv'], 'packages': [], 'resolve': None}, open('$SANDBOX/meta.json', 'w'))"
expect 2 "metadata without a resolved graph is exit 2, not a pass" "no resolved dependency graph"

# The server is a member with a package, but the resolve holds no node
# for it: a walk from it would cover nothing and pass.
python3 -c "import json; json.dump({'workspace_root': '/ws', 'workspace_members': ['srv'], 'packages': [{'id': 'srv', 'name': 'interweave-ipc-server', 'manifest_path': '/ws/crates/local/ipc-server/Cargo.toml'}], 'resolve': {'nodes': [{'id': 'other', 'deps': []}]}}, open('$SANDBOX/meta.json', 'w'))"
expect 2 "a guarded crate with no node in the graph is exit 2, not a pass" "has no node in the resolved graph"

# A dependency id with no package is metadata cargo never writes; the walk
# must call it a failure to check, not a breach.
python3 -c "import json; json.dump({'workspace_root': '/ws', 'workspace_members': ['srv'], 'packages': [{'id': 'srv', 'name': 'interweave-ipc-server', 'manifest_path': '/ws/crates/local/ipc-server/Cargo.toml'}], 'resolve': {'nodes': [{'id': 'srv', 'deps': [{'pkg': 'zz', 'dep_kinds': [{'kind': None}]}]}]}}, open('$SANDBOX/meta.json', 'w'))"
expect 2 "an unexpected error in the walk is exit 2, not a breach" "could not walk cargo metadata"

printf 'not json' > "$SANDBOX/meta.json"
expect 2 "output that is not JSON is exit 2, not a breach" "not JSON"

touch "$SANDBOX/cargo-fails"
expect 2 "cargo metadata failing is exit 2, and shows cargo's own error" "failed to load manifest"
rm -f "$SANDBOX/cargo-fails"

help_out="$(bash "$UNDER_TEST" --help 2>/dev/null)"
[[ "$help_out" == *"Plan §16's D2"* ]] && pass "--help prints the help block" || fail "--help should print the help block" "$help_out"

echo
if (( failures > 0 )); then
    echo "test_check_ipc_layering: $failures failure(s)" >&2
    exit 1
fi
echo "test_check_ipc_layering: OK — all assertions passed."
