#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/test_check_human_layering.sh
#
# Self-test for check_human_layering.sh.
#
# The guard reads `cargo metadata`, so these cases drive it through a stub
# `cargo` on PATH that prints a hand-built graph, and a sandbox workspace
# whose Cargo.toml carries the planned_members list. Each of the four rules
# has a case that must FAIL, and each carve-out (a dev-dependency, rusqlite
# outside ui-model, an app reaching Slint through ui-slint) one that must
# pass — the carve-outs are where a guard written too wide would go red on
# the tree, and too narrow would miss the breach next to them.
#
# Exit codes:
#   0  all assertions passed
#   1  one or more failed

set -uo pipefail

SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
UNDER_TEST="$SCRIPT_DIR/check_human_layering.sh"
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

PKGS='core interweave-human-core crates/human/core
proto interweave-human-chat-protocol crates/human/chat-protocol
store interweave-human-store crates/human/store
tc interweave-human-transport-client crates/human/transport-client
model interweave-human-ui-model crates/human/ui-model
uislint interweave-human-ui-slint crates/human/ui-slint
android interweave-human-android-platform crates/human/android-platform
desktop interweave-human-desktop apps/human-desktop
daemon interweave-transport-daemon apps/transport-daemon
api interweave-local-client-api crates/api/local-client-api
serde serde registry/serde
rt interweave-transport-runtime crates/transport/runtime
lp libp2p registry/libp2p
lpid libp2p-identity registry/libp2p-identity
slint slint registry/slint
islint i-slint-core registry/i-slint-core
sql rusqlite registry/rusqlite'
EDGES='core api normal
proto serde normal
store sql normal
store core normal'
FIVE_LATER="crates/human/transport-client crates/human/ui-model crates/human/ui-slint"
NOW="core proto store api"

echo "test_check_human_layering"

planned $FIVE_LATER
graph "$NOW" "$PKGS" "$EDGES"
expect 0 "today's crates, the rest planned, pass" "crates/human/ui-model is planned, not a member yet"
[[ "$(cat "$SANDBOX/cargo-args")" == *"--all-features"* ]] && pass "  and asks cargo for the all-features graph" \
    || fail "cargo metadata was not asked for --all-features" "$(cat "$SANDBOX/cargo-args")"

# Rule 1: transport, libp2p, Slint — direct, transitive, build.
graph "$NOW rt" "$PKGS" "$EDGES
core rt normal"
expect 1 "a human crate depending on crates/transport/* fails, with the path" "crates/human/core depends on a crate under crates/transport/: interweave-human-core -> interweave-transport-runtime"
graph "$NOW" "$PKGS" "$EDGES
api lpid normal"
expect 1 "a libp2p-* crate two hops down fails" "interweave-human-store -> interweave-human-core -> interweave-local-client-api -> libp2p-identity"
graph "$NOW" "$PKGS" "$EDGES
proto lp build"
expect 1 "a build-dependency counts" "-> libp2p"
graph "$NOW" "$PKGS" "$EDGES
serde islint normal"
expect 1 "an i-slint-* crate reached by chat-protocol fails" "crates/human/chat-protocol depends on a Slint crate"
graph "$NOW" "$PKGS" "$EDGES
core rt dev
core slint dev"
expect 0 "dev-dependencies are not followed"

# Rule 2: rusqlite is ui-model's ban, not store's.
graph "$NOW model" "$PKGS" "$EDGES
model core normal"
expect 0 "store may use rusqlite; ui-model on core passes"
graph "$NOW model" "$PKGS" "$EDGES
model store normal"
expect 1 "ui-model reaching rusqlite through store fails" "rusqlite (ui-model holds no storage)"

# Rule 3: another crates/human/* member reaching Slint.
graph "$NOW android" "$PKGS" "$EDGES
android serde normal
serde slint normal"
expect 1 "android-platform reaching Slint transitively fails" "crates/human/android-platform depends on a Slint crate"

# ui-slint and an app composing it are the carve-out.
graph "$NOW model uislint desktop" "$PKGS" "$EDGES
uislint slint normal
uislint model normal
model core normal
desktop uislint normal"
expect 0 "ui-slint on Slint, and an app reaching Slint only through ui-slint, pass"

# Rule 4: a direct Slint dependency outside ui-slint.
graph "$NOW model uislint desktop" "$PKGS" "$EDGES
uislint slint normal
desktop uislint normal
desktop slint normal"
expect 1 "an app declaring slint itself fails" "apps/human-desktop declares slint itself"
graph "$NOW daemon" "$PKGS" "$EDGES
daemon islint build"
expect 1 "any other member declaring a Slint crate fails" "apps/transport-daemon declares i-slint-core itself"

# Absent crates: planned is a skip, anything else a failure to check.
planned crates/human/ui-model crates/human/ui-slint
graph "$NOW" "$PKGS" "$EDGES"
expect 2 "a listed crate neither member nor planned is exit 2" "crates/human/transport-client is neither a workspace member nor in planned_members"
planned $FIVE_LATER
graph "core store api" "$PKGS" "$EDGES"
expect 2 "an existing crate dropped from the workspace is exit 2, not a pass" "crates/human/chat-protocol is neither"
printf '[workspace]\nmembers = []\n' > "$WS/Cargo.toml"
graph "$NOW" "$PKGS" "$EDGES"
expect 2 "no planned_members in the manifest is exit 2" "cannot read [workspace.metadata.interweave].planned_members"
planned $FIVE_LATER

python3 -c "import json; json.dump({'workspace_root': '$WS', 'workspace_members': [], 'packages': [], 'resolve': None}, open('$SANDBOX/meta.json', 'w'))"
expect 2 "metadata without a resolved graph is exit 2" "no resolved dependency graph"
python3 -c "import json; json.dump({'workspace_root': '$WS', 'workspace_members': ['core'], 'packages': [{'id': 'core', 'name': 'interweave-human-core', 'manifest_path': '$WS/crates/human/core/Cargo.toml'}], 'resolve': {'nodes': [{'id': 'other', 'deps': []}]}}, open('$SANDBOX/meta.json', 'w'))"
expect 2 "a guarded crate with no node in the graph is exit 2" "has no node in the resolved graph"
python3 -c "import json; json.dump({'workspace_root': '$WS', 'workspace_members': ['core'], 'packages': [{'id': 'core', 'name': 'interweave-human-core', 'manifest_path': '$WS/crates/human/core/Cargo.toml'}], 'resolve': {'nodes': [{'id': 'core', 'deps': [{'pkg': 'zz', 'dep_kinds': [{'kind': None}]}]}]}}, open('$SANDBOX/meta.json', 'w'))"
expect 2 "an unexpected error in the walk is exit 2, not a breach" "could not walk cargo metadata"
printf 'not json' > "$SANDBOX/meta.json"
expect 2 "output that is not JSON is exit 2" "not JSON"
touch "$SANDBOX/cargo-fails"
expect 2 "cargo metadata failing is exit 2, with cargo's own error" "failed to load manifest"
rm -f "$SANDBOX/cargo-fails"

help_out="$(bash "$UNDER_TEST" --help 2>/dev/null)"
[[ "$help_out" == *"Plan §17's P2"* ]] && pass "--help prints the help block" || fail "--help should print the help block" "$help_out"

echo
if (( failures > 0 )); then
    echo "test_check_human_layering: $failures failure(s)" >&2
    exit 1
fi
echo "test_check_human_layering: OK — all assertions passed."
