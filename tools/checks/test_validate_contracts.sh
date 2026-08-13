#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/test_validate_contracts.sh
#
# Self-test for validate_contracts.py. Each case builds a miniature
# schemas tree under $TMPDIR and runs the validator against it with
# --root, so no assertion depends on the real corpus.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECK="$SCRIPT_DIR/validate_contracts.py"

pass=0
fail=0
ok()  { printf '  ✓ %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf '  ✗ %s\n' "$1" >&2; fail=$((fail + 1)); }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# Output is captured, never piped into `grep -q`: with pipefail set, a
# `grep -q` that exits early kills the producer with EPIPE and the
# pipeline reports failure on output that was correct.
run()      { python3 "$CHECK" --root "$1" 2>&1; }
run_code() { python3 "$CHECK" --root "$1" >/dev/null 2>&1; printf '%s' "$?"; }

# make_tree <root> — one family, one conformant contract, plus the ADR
# and specification it cites.
make_tree() {
    local r="$1"
    mkdir -p "$r/architecture/adr" "$r/architecture/contracts/schemas/_meta" \
             "$r/architecture/contracts/schemas/demo"
    : > "$r/architecture/adr/0001-a-decision.md"
    : > "$r/architecture/contracts/DEMO.md"
    cp "$SCRIPT_DIR/../../architecture/contracts/schemas/_meta/contract.meta.schema.json" \
       "$r/architecture/contracts/schemas/_meta/"
    cat > "$r/architecture/contracts/schemas/manifest.json" <<'EOF'
{ "families": [ { "name": "demo", "path": "demo" } ] }
EOF
    cat > "$r/architecture/contracts/schemas/demo/manifest.json" <<'EOF'
{ "family": "demo", "concepts": [ { "name": "thing", "file": "thing.schema.json", "status": "approved" } ] }
EOF
    cat > "$r/architecture/contracts/schemas/demo/thing.schema.json" <<'EOF'
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "urn:interweave:schemas:demo:thing",
  "title": "A thing",
  "type": "string",
  "x-contract": {
    "name": "demo.thing",
    "status": "approved",
    "version": "1.0.0",
    "adr": ["0001"],
    "specification": "architecture/contracts/DEMO.md"
  }
}
EOF
}

edit() { python3 -c "
import json,sys
p=sys.argv[1]; d=json.load(open(p))
exec(sys.argv[2])
json.dump(d, open(p,'w'), indent=2)
" "$1" "$2"; }

printf 'test_validate_contracts\n'

# ── a conformant tree passes ─────────────────────────────────────────────
R="$TMP/clean"; make_tree "$R"
[ "$(run_code "$R")" = "0" ] && ok "conformant tree exits 0" || bad "should pass: $(run "$R")"

# ── a schema missing from the family manifest ────────────────────────────
R="$TMP/unmanifested"; make_tree "$R"
cp "$R/architecture/contracts/schemas/demo/thing.schema.json" \
   "$R/architecture/contracts/schemas/demo/other.schema.json"
out="$(run "$R")"
[[ "$out" == *"no concept row in the family manifest"* ]] && ok "an unmanifested schema is reported" || bad "should report the missing row"

# ── a manifest row naming a file that does not exist ─────────────────────
R="$TMP/ghostrow"; make_tree "$R"
edit "$R/architecture/contracts/schemas/demo/manifest.json" \
     "d['concepts'].append({'name':'ghost','file':'ghost.schema.json','status':'approved'})"
out="$(run "$R")"
[[ "$out" == *"which does not exist"* ]] && ok "a row for a missing file is reported" || bad "should report the dangling row"

# ── a family directory with no root-manifest row ─────────────────────────
R="$TMP/unlisted"; make_tree "$R"
mkdir -p "$R/architecture/contracts/schemas/extra"
out="$(run "$R")"
[[ "$out" == *"no row in the root manifest"* ]] && ok "an unlisted family directory is reported" || bad "should report the unlisted family"

# ── $id disagreeing with its location ────────────────────────────────────
R="$TMP/badid"; make_tree "$R"
edit "$R/architecture/contracts/schemas/demo/thing.schema.json" \
     "d['\$id']='urn:interweave:schemas:elsewhere:thing'; d['x-contract']['name']='elsewhere.thing'"
out="$(run "$R")"
[[ "$out" == *"does not match directory"* ]] && ok "an \$id in the wrong family is reported" || bad "should report the family mismatch"

R="$TMP/badconcept"; make_tree "$R"
edit "$R/architecture/contracts/schemas/demo/thing.schema.json" \
     "d['\$id']='urn:interweave:schemas:demo:other'; d['x-contract']['name']='demo.other'"
out="$(run "$R")"
[[ "$out" == *"implies other.schema.json"* ]] && ok "an \$id concept not matching the filename is reported" || bad "should report the filename mismatch"

# ── x-contract.name not the dotted form of $id ───────────────────────────
R="$TMP/badname"; make_tree "$R"
edit "$R/architecture/contracts/schemas/demo/thing.schema.json" "d['x-contract']['name']='demo.wrong'"
out="$(run "$R")"
[[ "$out" == *"should be 'demo.thing'"* ]] && ok "a wrong x-contract.name is reported" || bad "should report the dotted-name mismatch"

# ── status disagreeing between manifest and schema ───────────────────────
R="$TMP/status"; make_tree "$R"
edit "$R/architecture/contracts/schemas/demo/manifest.json" "d['concepts'][0]['status']='active'"
out="$(run "$R")"
[[ "$out" == *"must agree"* ]] && ok "a lifecycle mismatch between manifest and schema is reported" || bad "should report the status mismatch"

# ── an invalid status value fails the meta-schema ────────────────────────
R="$TMP/badstatus"; make_tree "$R"
edit "$R/architecture/contracts/schemas/demo/thing.schema.json" "d['x-contract']['status']='invented'"
edit "$R/architecture/contracts/schemas/demo/manifest.json" "d['concepts'][0]['status']='invented'"
out="$(run "$R")"
[[ "$out" == *"fails the meta-schema"* ]] && ok "an invented status fails the meta-schema" || bad "should reject the invented status"

# ── a missing x-contract block entirely ──────────────────────────────────
R="$TMP/nocontract"; make_tree "$R"
edit "$R/architecture/contracts/schemas/demo/thing.schema.json" "d.pop('x-contract')"
out="$(run "$R")"
[[ "$out" == *"fails the meta-schema"* ]] && ok "a schema with no x-contract block is rejected" || bad "should reject the missing envelope"

# ── provenance: a cited ADR that does not exist ──────────────────────────
R="$TMP/badadr"; make_tree "$R"
edit "$R/architecture/contracts/schemas/demo/thing.schema.json" "d['x-contract']['adr']=['9999']"
out="$(run "$R")"
[[ "$out" == *"cites ADR-9999"* ]] && ok "a citation of a nonexistent ADR is reported" || bad "should report the missing ADR"

# ── provenance: a specification path that does not exist ─────────────────
R="$TMP/badspec"; make_tree "$R"
edit "$R/architecture/contracts/schemas/demo/thing.schema.json" \
     "d['x-contract']['specification']='architecture/contracts/NOPE.md'"
out="$(run "$R")"
[[ "$out" == *"does not exist"* ]] && ok "a missing specification file is reported" || bad "should report the missing spec"

# ── a \$ref to a contract that is not in the corpus ──────────────────────
R="$TMP/badref"; make_tree "$R"
edit "$R/architecture/contracts/schemas/demo/thing.schema.json" \
     "d.pop('type'); d['properties']={'x':{'\$ref':'urn:interweave:schemas:demo:absent'}}; d['type']='object'"
out="$(run "$R")"
[[ "$out" == *"resolves to no schema"* ]] && ok "a dangling \$ref is reported" || bad "should report the dangling ref"

# ── an illegal JSON Schema is caught as well as meta-invalidity ──────────
R="$TMP/illegal"; make_tree "$R"
edit "$R/architecture/contracts/schemas/demo/thing.schema.json" "d['type']=['not-a-type']"
out="$(run "$R")"
[[ "$out" == *"not a legal Draft 2020-12 schema"* ]] && ok "an illegal schema is reported" || bad "should report the illegal schema"

# ── malformed JSON ───────────────────────────────────────────────────────
R="$TMP/malformed"; make_tree "$R"
printf '{ not json' > "$R/architecture/contracts/schemas/demo/thing.schema.json"
out="$(run "$R")"
[[ "$out" == *"invalid JSON"* ]] && ok "malformed JSON is reported" || bad "should report invalid JSON"

# ── the real corpus is clean ─────────────────────────────────────────────
REAL="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel 2>/dev/null)"
if [ -n "$REAL" ]; then
    [ "$(run_code "$REAL")" = "0" ] && ok "the real contract corpus validates" || bad "real corpus fails: $(run "$REAL")"
fi

# ── usage ────────────────────────────────────────────────────────────────
[ "$(run_code "$TMP/nothing-here")" = "2" ] && ok "a missing tree exits 2" || bad "missing tree should exit 2"
help_out="$(python3 "$CHECK" --help 2>/dev/null)"
[[ "$help_out" == *"MANIFEST"* ]] && ok "--help prints the help block" || bad "--help should print help"

printf '\n'
if [ "$fail" -gt 0 ]; then
    printf 'test_validate_contracts: %d passed, %d FAILED.\n' "$pass" "$fail" >&2
    exit 1
fi
printf 'test_validate_contracts: OK — all %d assertions passed.\n' "$pass"
