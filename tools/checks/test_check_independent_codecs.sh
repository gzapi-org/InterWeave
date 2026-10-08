#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/test_check_independent_codecs.sh
#
# Self-test for check_independent_codecs.sh, through a stub `cargo` on
# PATH that prints a hand-built graph: what is under test is how the guard
# WALKS it. The cases that matter most are the two the layering checks
# would pass — a dev-dependency on a path package at the first hop, and a
# path package reached through a registry crate.
#
# Exit codes:
#   0  all assertions passed
#   1  one or more failed

set -uo pipefail

SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
UNDER_TEST="$SCRIPT_DIR/check_independent_codecs.sh"
[[ -f "$UNDER_TEST" ]] || { echo "test: $UNDER_TEST not found" >&2; exit 1; }

failures=0
SANDBOX="$(mktemp -d)"
trap 'rm -rf "$SANDBOX"' EXIT

pass() { echo "  ✓ $1"; }
fail() { echo "  ✗ $1" >&2; printf '%s\n' "${2:-}" | sed 's/^/      /' >&2
         failures=$((failures + 1)); }

# graph <members> <package lines> <edge lines>. A package line is
# `id name path|registry`; an edge line is `from to kind`, kind normal,
# build or dev.
graph() {
    python3 - "$SANDBOX/meta.json" "$1" "$2" "$3" <<'PYEOF'
import json, sys
out, members, pkgs, edges = sys.argv[1:5]
packages, nodes = [], {}
for line in filter(None, pkgs.split("\n")):
    pid, name, src = line.split()
    packages.append({"id": pid, "name": name,
                     "source": None if src == "path" else "registry+https://github.com/rust-lang/crates.io-index"})
    nodes[pid] = {"id": pid, "deps": []}
for line in filter(None, edges.split("\n")):
    a, b, kind = line.split()
    nodes[a]["deps"].append({"pkg": b, "dep_kinds": [{"kind": None if kind == "normal" else kind}]})
json.dump({"workspace_members": members.split(), "packages": packages,
           "resolve": {"nodes": list(nodes.values())}}, open(out, "w"))
PYEOF
}

mkdir -p "$SANDBOX/bin"
cat > "$SANDBOX/bin/cargo" <<EOF
#!/usr/bin/env bash
echo "    Updating crates.io index" >&2
printf '%s\n' "\$*" > "$SANDBOX/cargo-args"
[[ -e "$SANDBOX/cargo-fails" ]] && { echo "error: failed to load manifest" >&2; exit 101; }
cat "$SANDBOX/meta.json"
EOF
chmod +x "$SANDBOX/bin/cargo"

expect() {
    local want="$1" name="$2" says="${3:-}" out got
    out="$(PATH="$SANDBOX/bin:$PATH" bash "$UNDER_TEST" 2>&1)"; got=$?
    if [[ "$got" -ne "$want" ]]; then fail "$name — wanted exit $want, got $got" "$out"; return; fi
    if [[ -n "$says" && "$out" != *"$says"* ]]; then fail "$name — output lacks: $says" "$out"; return; fi
    pass "$name (exit $got)"
}

PKGS='ic interweave-independent-codecs path
sha sha2 registry
dig digest registry
sj serde_json registry
sup interweave-test-support path
lp2 interweave-transport-libp2p path
vend libp2p-autonat path'
EDGES='ic sha normal
sha dig normal
ic sj dev'
MEMBERS="ic sup lp2"

echo "test_check_independent_codecs"

graph "$MEMBERS" "$PKGS" "$EDGES"
expect 0 "registry crates alone, dev included, pass" "reaches no path package"
[[ "$(cat "$SANDBOX/cargo-args")" == *"--all-features"* ]] && pass "  and asks cargo for the all-features graph" \
    || fail "cargo metadata was not asked for --all-features" "$(cat "$SANDBOX/cargo-args")"

graph "$MEMBERS" "$PKGS" "$EDGES
ic lp2 normal"
expect 1 "a normal dependency on a production crate fails" "interweave-independent-codecs -> interweave-transport-libp2p"

graph "$MEMBERS" "$PKGS" "$EDGES
ic sup dev"
expect 1 "a DEV-dependency on a path package fails — the layering checks would pass it" "-> interweave-test-support"

graph "$MEMBERS" "$PKGS" "$EDGES
ic lp2 build"
expect 1 "a build-dependency fails" "interweave-transport-libp2p"

graph "$MEMBERS" "$PKGS" "$EDGES
dig lp2 normal"
expect 1 "a path package reached through a registry crate fails, with the path" "interweave-independent-codecs -> sha2 -> digest -> interweave-transport-libp2p"

graph "$MEMBERS" "$PKGS" "$EDGES
ic vend normal"
expect 1 "a vendored path package that is no workspace member fails" "-> libp2p-autonat"

graph "$MEMBERS" "$PKGS" "$EDGES
sha sup dev"
expect 0 "a dependency's own dev-dependency is not followed"

graph "sup lp2" "$PKGS" "$EDGES"
expect 2 "no codecs crate in the workspace is a failure to check" "not a workspace member"

python3 -c "import json; json.dump({'workspace_members': ['ic'], 'packages': [], 'resolve': None}, open('$SANDBOX/meta.json', 'w'))"
expect 2 "metadata without a resolved graph is exit 2" "no resolved dependency graph"

python3 -c "import json; json.dump({'workspace_members': ['ic'], 'packages': [{'id': 'ic', 'name': 'interweave-independent-codecs', 'source': None}], 'resolve': {'nodes': [{'id': 'other', 'deps': []}]}}, open('$SANDBOX/meta.json', 'w'))"
expect 2 "the crate with no node in the graph is exit 2" "has no node in the resolved graph"

python3 -c "import json; json.dump({'workspace_members': ['ic'], 'packages': [{'id': 'ic', 'name': 'interweave-independent-codecs', 'source': None}], 'resolve': {'nodes': [{'id': 'ic', 'deps': [{'pkg': 'zz', 'dep_kinds': [{'kind': None}]}]}]}}, open('$SANDBOX/meta.json', 'w'))"
expect 2 "an unexpected error in the walk is exit 2, not a breach" "could not walk cargo metadata"

printf 'not json' > "$SANDBOX/meta.json"
expect 2 "output that is not JSON is exit 2" "not JSON"

touch "$SANDBOX/cargo-fails"
expect 2 "cargo metadata failing is exit 2, with cargo's error" "failed to load manifest"
rm -f "$SANDBOX/cargo-fails"

help_out="$(bash "$UNDER_TEST" --help 2>/dev/null)"
[[ "$help_out" == *"independent codecs stay independent"* ]] && pass "--help prints the help block" || fail "--help should print the help block" "$help_out"

echo
if (( failures > 0 )); then
    echo "test_check_independent_codecs: $failures failure(s)" >&2
    exit 1
fi
echo "test_check_independent_codecs: OK — all assertions passed."
