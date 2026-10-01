#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/test_check_bridge_default_features.sh
#
# Self-test for check_bridge_default_features.sh.
#
# A stub `cargo` on PATH answers `cargo metadata` from a hand-built member
# list and `cargo tree -p <pkg>` from a per-package file, so each case says
# exactly which graph a bridge package resolves. The cases that matter most
# are the positive control (a tree that is empty or names another package
# must not pass) and the exact-name match (a crate whose name merely starts
# with a parser's must not fail).
#
# Exit codes:
#   0  all assertions passed
#   1  one or more failed

set -uo pipefail

SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
UNDER_TEST="$SCRIPT_DIR/check_bridge_default_features.sh"
[[ -f "$UNDER_TEST" ]] || { echo "test: $UNDER_TEST not found" >&2; exit 1; }

failures=0
SANDBOX="$(mktemp -d)"
trap 'rm -rf "$SANDBOX"' EXIT
WS="$SANDBOX/ws"
mkdir -p "$WS" "$SANDBOX/bin" "$SANDBOX/trees"

pass() { echo "  ✓ $1"; }
fail() { echo "  ✗ $1" >&2; printf '%s\n' "${2:-}" | sed 's/^/      /' >&2
         failures=$((failures + 1)); }

planned() {
    { printf '[workspace]\nmembers = []\n\n[workspace.metadata.interweave]\nplanned_members = [\n'
      for d in "$@"; do printf '  "%s",\n' "$d"; done
      printf ']\n'; } > "$WS/Cargo.toml"
}

# members "<dir> <name>"…: the workspace members cargo metadata reports.
members() {
    python3 - "$SANDBOX/meta.json" "$WS" "$@" <<'PYEOF'
import json, sys
out, ws, *rows = sys.argv[1:]
pk = []
for r in rows:
    d, n = r.split()
    pk.append({"id": n, "name": n, "manifest_path": f"{ws}/{d}/Cargo.toml"})
json.dump({"workspace_root": ws, "workspace_members": [p["id"] for p in pk], "packages": pk}, open(out, "w"))
PYEOF
}

# tree <pkg> <package names…>: what `cargo tree -p <pkg>` prints, root first.
tree() {
    local pkg="$1"; shift
    : > "$SANDBOX/trees/$pkg"
    for n in "$@"; do echo "$n v1.0.0" >> "$SANDBOX/trees/$pkg"; done
}

cat > "$SANDBOX/bin/cargo" <<EOF
#!/usr/bin/env bash
echo "    Updating crates.io index" >&2
case "\$1" in
  metadata)
    [[ -e "$SANDBOX/metadata-fails" ]] && { echo "error: failed to load manifest" >&2; exit 101; }
    cat "$SANDBOX/meta.json" ;;
  tree)
    printf '%s\n' "\$*" >> "$SANDBOX/tree-args"
    [[ -e "$SANDBOX/tree-fails" ]] && { echo "error: package ID specification matched no packages" >&2; exit 101; }
    pkg=""; while (( \$# )); do [[ "\$1" == -p ]] && { pkg="\$2"; shift; }; shift; done
    cat "$SANDBOX/trees/\$pkg" ;;
esac
EOF
chmod +x "$SANDBOX/bin/cargo"

expect() {
    local want="$1" name="$2" says="${3:-}" out got
    out="$(PATH="$SANDBOX/bin:$PATH" bash "$UNDER_TEST" 2>&1)"; got=$?
    if [[ "$got" -ne "$want" ]]; then fail "$name — wanted exit $want, got $got" "$out"; return; fi
    if [[ -n "$says" && "$out" != *"$says"* ]]; then fail "$name — output lacks: $says" "$out"; return; fi
    pass "$name (exit $got)"
}

CORE="crates/claude/channel-core interweave-claude-channel-core"
APP="apps/claude-channel interweave-claude-channel"

echo "test_check_bridge_default_features"

planned crates/claude/channel-core apps/claude-channel
members "crates/human/core interweave-human-core"
expect 0 "both bridge packages planned and absent pass, said" "apps/claude-channel is planned, not a member yet"

members "$CORE" "$APP" "crates/human/chat-protocol interweave-human-chat-protocol"
tree interweave-claude-channel-core interweave-claude-channel-core interweave-human-chat-protocol serde
tree interweave-claude-channel interweave-claude-channel interweave-claude-channel-core tokio
rm -f "$SANDBOX/tree-args"
expect 0 "bridge trees without a parser pass" "interweave-claude-channel) names no CommonMark parser"
# `-e normal` as a whole edge list (not `normal,dev`), and `--prefix none`:
# without it real cargo indents with box-drawing characters, the first
# field is `├──`, and no parser could ever match while the root line —
# unindented — still satisfied the positive control.
grep -qE -- "(^| )-e normal( |$)" "$SANDBOX/tree-args" && grep -q -- "--prefix none" "$SANDBOX/tree-args" \
    && ! grep -qE -- "--all-features|--features" "$SANDBOX/tree-args" \
    && pass "  and each tree is asked with -e normal and --prefix none under default features" \
    || fail "cargo tree was not asked for the default-feature normal graph" "$(cat "$SANDBOX/tree-args")"
[[ "$(grep -c -- '-p ' "$SANDBOX/tree-args")" -eq 2 ]] && pass "  and per package, not the workspace" \
    || fail "cargo tree was not run once per bridge package" "$(cat "$SANDBOX/tree-args")"
grep -q 'chat-protocol' "$SANDBOX/tree-args" && fail "a non-bridge member was checked" "$(cat "$SANDBOX/tree-args")" \
    || pass "  and only the bridge's packages"

tree interweave-claude-channel-core interweave-claude-channel-core interweave-human-chat-protocol pulldown-cmark
expect 1 "pulldown-cmark in channel-core's default graph fails" "crates/claude/channel-core (interweave-claude-channel-core) names a CommonMark parser under default features: pulldown-cmark"
tree interweave-claude-channel-core interweave-claude-channel-core serde
tree interweave-claude-channel interweave-claude-channel comrak
expect 1 "comrak reached by the composition root fails" "apps/claude-channel (interweave-claude-channel) names a CommonMark parser under default features: comrak"
tree interweave-claude-channel interweave-claude-channel tokio

members "$CORE" "$APP" "crates/claude/extra interweave-claude-extra"
tree interweave-claude-extra interweave-claude-extra markdown
expect 1 "a new crate under crates/claude/ is checked without being named" "crates/claude/extra (interweave-claude-extra) names a CommonMark parser under default features: markdown"
tree interweave-claude-extra interweave-claude-extra pulldown-cmark-escape markdown-table
expect 0 "a crate whose name only starts with a parser's passes"

members "$CORE" "$APP"
tree interweave-claude-channel interweave-claude-channel-core tokio
expect 2 "a tree rooted at another package is exit 2 (positive control)" "did not name interweave-claude-channel as its root"
: > "$SANDBOX/trees/interweave-claude-channel"
expect 2 "an empty tree is exit 2 (positive control)" "got 'nothing'"
tree interweave-claude-channel interweave-claude-channel tokio

planned apps/claude-channel
members "$APP"
expect 2 "channel-core neither member nor planned is exit 2" "crates/claude/channel-core is neither a workspace member nor in planned_members"
planned crates/claude/channel-core apps/claude-channel
printf '[workspace]\nmembers = []\n' > "$WS/Cargo.toml"
expect 2 "no planned_members in the manifest is exit 2" "cannot read the workspace members or planned_members"
planned crates/claude/channel-core apps/claude-channel

members "$CORE" "$APP"
touch "$SANDBOX/tree-fails"
expect 2 "cargo tree failing is exit 2, with cargo's error" "matched no packages"
rm -f "$SANDBOX/tree-fails"
touch "$SANDBOX/metadata-fails"
expect 2 "cargo metadata failing is exit 2, with cargo's error" "failed to load manifest"
rm -f "$SANDBOX/metadata-fails"

help_out="$(bash "$UNDER_TEST" --help 2>/dev/null)"
[[ "$help_out" == *"POSITIVE CONTROL"* ]] && pass "--help prints the help block, positive control included" || fail "--help should print the help block" "$help_out"

echo
if (( failures > 0 )); then
    echo "test_check_bridge_default_features: $failures failure(s)" >&2
    exit 1
fi
echo "test_check_bridge_default_features: OK — all assertions passed."
