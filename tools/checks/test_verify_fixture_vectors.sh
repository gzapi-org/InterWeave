#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/test_verify_fixture_vectors.sh
#
# Self-test for verify_fixture_vectors.py. Builds throwaway fixture trees
# under $TMPDIR so no assertion depends on the real vectors — except the
# one that deliberately checks them.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECK="$SCRIPT_DIR/verify_fixture_vectors.py"

pass=0
fail=0
ok()  { printf '  ✓ %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf '  ✗ %s\n' "$1" >&2; fail=$((fail + 1)); }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# Captured, never piped into `grep -q` — pipefail plus an early-exiting
# grep turns correct output into a failed pipeline.
run()      { python3 "$CHECK" --root "$1" 2>&1; }
run_code() { python3 "$CHECK" --root "$1" >/dev/null 2>&1; printf '%s' "$?"; }

# The golden from ADR-0047, independently restated here.
GOLDEN="d73342f033f00fca9c4ffcced6f9e6debaeb53e3743049ee9aaf227a55f9bf15"

make_fixture() {
    local r="$1" sha="$2"
    mkdir -p "$r/fixtures/direct-v2"
    # The anchor ADR must exist: a fixture citing a decision that is not
    # there is one of the things the checker reports.
    mkdir -p "$r/architecture/adr"
    : > "$r/architecture/adr/0047-namespace.md"
    cat > "$r/fixtures/direct-v2/f.json" <<EOF
{
  "algorithm": { "id": "direct-content-fingerprint-v1" },
  "vectors": [
    {
      "name": "golden-text-plain-hello",
      "frozen_by": "0047",
      "media_type": "text/plain",
      "payload_hex": "68656c6c6f",
      "sha256": "$sha"
    }
  ]
}
EOF
}

printf 'test_verify_fixture_vectors\n'

# ── a correct vector passes ──────────────────────────────────────────────
R="$TMP/clean"; make_fixture "$R" "$GOLDEN"
[ "$(run_code "$R")" = "0" ] && ok "a correct vector recomputes and passes" || bad "should pass: $(run "$R")"

# ── a drifted hash is caught ─────────────────────────────────────────────
R="$TMP/drift"; make_fixture "$R" "0000000000000000000000000000000000000000000000000000000000000000"
out="$(run "$R")"
[ "$(run_code "$R")" = "1" ] && ok "a drifted hash exits 1" || bad "drift should exit 1"
[[ "$out" == *"DRIFT"* && "$out" == *"$GOLDEN"* ]] && ok "  and prints stored vs computed" || bad "should show both values"

# ── this is the check that would catch a namespace change ────────────────
# The domain prefix participates in the hash, so a fixture frozen under a
# different project namespace cannot silently survive.
R="$TMP/namespace"; make_fixture "$R" "$GOLDEN"
python3 - "$R/fixtures/direct-v2/f.json" <<'PY'
import json,sys,hashlib
p=sys.argv[1]; d=json.load(open(p))
old = b"someotherproject/direct-content-fingerprint/v1\x00"
buf = old + b"\x01" + (10).to_bytes(2,'big') + b"text/plain" + (5).to_bytes(4,'big') + b"hello"
d['vectors'][0]['sha256'] = hashlib.sha256(buf).hexdigest()
json.dump(d, open(p,'w'))
PY
[ "$(run_code "$R")" = "1" ] && ok "a vector frozen under another namespace is caught" || bad "namespace drift should fail"

# ── an unknown algorithm is a failure, not a skip ────────────────────────
R="$TMP/unknown"; make_fixture "$R" "$GOLDEN"
python3 -c "
import json,sys
p=sys.argv[1]; d=json.load(open(p)); d['algorithm']['id']='some-future-thing'
json.dump(d, open(p,'w'))" "$R/fixtures/direct-v2/f.json"
out="$(run "$R")"
[[ "$out" == *"cannot compute"* ]] && ok "an unverifiable algorithm is reported, not skipped" || bad "unknown algorithm should fail"

# ── two vectors hashing identically is a collision ───────────────────────
R="$TMP/collide"; make_fixture "$R" "$GOLDEN"
python3 -c "
import json,sys,copy
p=sys.argv[1]; d=json.load(open(p))
dup=copy.deepcopy(d['vectors'][0]); dup['name']='twin'; dup.pop('frozen_by')
d['vectors'].append(dup)
json.dump(d, open(p,'w'))" "$R/fixtures/direct-v2/f.json"
out="$(run "$R")"
[[ "$out" == *"collides with"* ]] && ok "a duplicate fingerprint is reported as a collision" || bad "should report the collision"

# ── but a VALIDATION verdict set is allowed to repeat its result ─────────
# A grammar file exists to map many inputs onto `true` and many onto
# `false`. Applying the derivation collision rule there would make a
# correct verdict set unrepresentable, so distinctness is per-algorithm.
R="$TMP/verdicts"; mkdir -p "$R/fixtures/endpoints" "$R/architecture/adr"
touch "$R/architecture/adr/0030-x.md"
cat > "$R/fixtures/endpoints/g.json" <<'EOF'
{
  "algorithm": { "id": "endpoint-id-grammar-v1" },
  "adr": ["0030"],
  "vectors": [
    { "name": "lower", "endpoint_id": "human", "valid": true },
    { "name": "dotted", "endpoint_id": "automation.build", "valid": true },
    { "name": "upper", "endpoint_id": "Human", "valid": false },
    { "name": "empty", "endpoint_id": "", "valid": false }
  ]
}
EOF
out="$(run "$R")"
[ "$(run_code "$R")" = "0" ] && ok "repeated verdicts are not treated as collisions" \
    || bad "a verdict set should pass: $out"

# ── the endpoint-directory frame: 33 entries cannot be encoded ───────────
# The frame carries at most 32; the verifier refuses to compute a 33rd
# rather than truncating, so a fixture cannot freeze an illegal frame.
R="$TMP/directory"; mkdir -p "$R/fixtures/endpoints" "$R/architecture/adr"
touch "$R/architecture/adr/0031-x.md"
python3 -c "
import json,sys
p=sys.argv[1]
ok={'name':'one','kind':'directory','generated_at_ms':1786600000000,'ttl_ms':60000,'endpoints':['human'],
    'frame_hex':'010000019ff9a88a000000ea60010568756d616e','frame_len':20}
too={'name':'too-many','kind':'directory','generated_at_ms':0,'ttl_ms':0,
     'endpoints':['e%02d'%i for i in range(33)],'frame_hex':'00'}
json.dump({'algorithm':{'id':'endpoint-directory-v1-frame'},'adr':['0031'],'vectors':[ok,too]}, open(p,'w'))
" "$R/fixtures/endpoints/d.json"
out="$(run "$R")"
[[ "$out" == *"at most 32"* ]] && ok "a 33-entry directory frame is refused, not truncated" || bad "should refuse 33 entries: $out"
python3 -c "
import json,sys
p=sys.argv[1]; d=json.load(open(p)); d['vectors'].pop(); json.dump(d, open(p,'w'))" "$R/fixtures/endpoints/d.json"
[ "$(run_code "$R")" = "0" ] && ok "a legal directory frame recomputes" || bad "the one-entry frame should recompute: $(run "$R")"

# ── a file with no ADR anchor ────────────────────────────────────────────
R="$TMP/unanchored"; make_fixture "$R" "$GOLDEN"
python3 -c "
import json,sys
p=sys.argv[1]; d=json.load(open(p)); d['vectors'][0].pop('frozen_by')
json.dump(d, open(p,'w'))" "$R/fixtures/direct-v2/f.json"
out="$(run "$R")"
[[ "$out" == *"nothing anchors this file"* ]] && ok "a fixture with no anchor at all is reported" || bad "should require an anchor"

# ── an out-of-range media type cannot be computed ────────────────────────
R="$TMP/badmedia"; make_fixture "$R" "$GOLDEN"
python3 -c "
import json,sys
p=sys.argv[1]; d=json.load(open(p)); d['vectors'][0]['media_type']=''
json.dump(d, open(p,'w'))" "$R/fixtures/direct-v2/f.json"
out="$(run "$R")"
[[ "$out" == *"empty is invalid rather than absent"* ]] && ok "an empty media type is rejected, not treated as absent" || bad "should reject empty media"

# ── non-vector JSON under fixtures/ is ignored ───────────────────────────
R="$TMP/other"; make_fixture "$R" "$GOLDEN"
printf '{"note":"not a vector file"}\n' > "$R/fixtures/direct-v2/notes.json"
[ "$(run_code "$R")" = "0" ] && ok "JSON without a vectors array is ignored" || bad "non-vector JSON should be ignored"

# ── anchoring: a file-level `adr` is a valid anchor on its own ───────────
# A layout with no ADR-published golden derives every vector from the
# decided algorithm. Demanding `frozen_by` there would force inventing a
# golden or going unanchored — the second being what this guards against.
R="$TMP/anchor-file"; make_fixture "$R" "$GOLDEN"
mkdir -p "$R/architecture/adr"; : > "$R/architecture/adr/0019-dedup.md"
python3 -c "
import json,sys
p=sys.argv[1]; d=json.load(open(p))
d['adr']=['0019']; d['vectors'][0].pop('frozen_by')
json.dump(d, open(p,'w'))" "$R/fixtures/direct-v2/f.json"
[ "$(run_code "$R")" = "0" ] && ok "a file-level adr anchors a derived-vector file" || bad "file-level adr should anchor: $(run "$R")"

# ── ...but a cited ADR that does not exist is caught ─────────────────────
R="$TMP/anchor-ghost"; make_fixture "$R" "$GOLDEN"
mkdir -p "$R/architecture/adr"
python3 -c "
import json,sys
p=sys.argv[1]; d=json.load(open(p))
d['adr']=['9999']; d['vectors'][0].pop('frozen_by')
json.dump(d, open(p,'w'))" "$R/fixtures/direct-v2/f.json"
out="$(run "$R")"
[[ "$out" == *"ADR-9999"* ]] && ok "a nonexistent anchor ADR is reported" || bad "should report the missing ADR"

# ── neither anchor at all is still a failure ─────────────────────────────
R="$TMP/anchor-none"; make_fixture "$R" "$GOLDEN"
python3 -c "
import json,sys
p=sys.argv[1]; d=json.load(open(p)); d['vectors'][0].pop('frozen_by')
json.dump(d, open(p,'w'))" "$R/fixtures/direct-v2/f.json"
out="$(run "$R")"
[[ "$out" == *"nothing anchors this file"* ]] && ok "an unanchored file is still reported" || bad "should require an anchor"

# ── an algorithm freezing two fields has BOTH verified ───────────────────
# The Kademlia fixture publishes `network_hash` and `protocol` on the
# same authority. Only the hash used to be recomputed, so replacing a
# golden protocol with anything at all still passed and consumers read an
# unchecked value as frozen.
make_kad() {
    local r="$1" proto="$2"
    mkdir -p "$r/fixtures/kademlia" "$r/architecture/adr"
    : > "$r/architecture/adr/0047-namespace.md"
    cat > "$r/fixtures/kademlia/k.json" <<EOF
{
  "algorithm": { "id": "kad-network-namespace-v1" },
  "adr": ["0047"],
  "vectors": [
    {
      "name": "single-character",
      "network_id": "a",
      "network_hash": "ygneka5pm3tlc4zypofzfj4vsq",
      "protocol": "$proto"
    }
  ]
}
EOF
}

R="$TMP/kad-ok"; make_kad "$R" "/interweave/kad/1.0.0/ygneka5pm3tlc4zypofzfj4vsq"
[ "$(run_code "$R")" = "0" ] && ok "both frozen fields recompute" || bad "should pass: $(run "$R")"

R="$TMP/kad-bad"; make_kad "$R" "/definitely/wrong"
out="$(run "$R")"
[ "$(run_code "$R")" = "1" ] && ok "a wrong protocol string exits 1" || bad "an unchecked golden should fail"
[[ "$out" == *"DRIFT in protocol"* ]] && ok "  and names which field drifted" || bad "should name the field: $out"

# ── a golden with no usable marker is reported, not skipped ──────────────
# The prose scan can only attribute a quoted hash to a vector through the
# vector's own inputs. When none of the declared marker fields are
# present it matches nothing — and used to count that as a scanned file,
# so a stale hash read as covered.
R="$TMP/nomarker"
mkdir -p "$R/fixtures/gossipsub" "$R/architecture/adr"
: > "$R/architecture/adr/0047-namespace.md"
cat > "$R/fixtures/gossipsub/g.json" <<'EOF'
{
  "algorithm": { "id": "gossipsub-topic-key-v1" },
  "adr": ["0047"],
  "vectors": [
    {
      "name": "general",
      "frozen_by": "0047",
      "channel_id": "general",
      "sha256": "82695daad230a8a8ddb6e43aae1063e4f611ded53d710f48b2ed3d206211c3bc"
    }
  ]
}
EOF
printf 'ChannelId  = general\nSHA-256    = 82695daad230a8a8ddb6e43aae1063e4f611ded53d710f48b2ed3d206211c3bc\n' > "$R/PUBSUB.md"
[ "$(run_code "$R")" = "0" ] && ok "a correct prose copy of a non-payload golden passes" || bad "should pass: $(run "$R")"

printf 'ChannelId  = general\nSHA-256    = 0000000000000000000000000000000000000000000000000000000000000000\n' > "$R/PUBSUB.md"
out="$(run "$R")"
[ "$(run_code "$R")" = "1" ] && ok "a stale copy of a non-payload golden is caught" || bad "the prose scan must see channel_id-keyed goldens: $out"

# ── a prose copy that drifted is caught ──────────────────────────────────
# Only the vector file is recomputed, so a re-freeze would otherwise
# leave every quoted copy confidently wrong — in the ADRs and contracts a
# reader trusts most. ADR-0047 re-froze these values once already.
R="$TMP/prose"; make_fixture "$R" "$GOLDEN"
mkdir -p "$R/architecture/contracts"
printf 'media_type = "text/plain"\npayload    = UTF8("hello")\nSHA-256    = %s\n' \
    "0000000000000000000000000000000000000000000000000000000000000000" \
    > "$R/architecture/contracts/SPEC.md"
out="$(run "$R")"
[ "$(run_code "$R")" = "1" ] && ok "a stale prose copy exits 1" || bad "stale prose copy should fail"
[[ "$out" == *"SPEC.md:3"* ]] && ok "  and names the file and line" || bad "should name SPEC.md:3"

# ── a prose copy quoting the CORRECT value passes ────────────────────────
R="$TMP/prose-ok"; make_fixture "$R" "$GOLDEN"
mkdir -p "$R/architecture/contracts"
printf 'media_type = "text/plain"\npayload    = UTF8("hello")\nSHA-256    = %s\n' "$GOLDEN" \
    > "$R/architecture/contracts/SPEC.md"
[ "$(run_code "$R")" = "0" ] && ok "a correct prose copy passes" || bad "correct prose copy should pass: $(run "$R")"

# ── quoting ANOTHER vector's hash beside these inputs is still wrong ─────
# Hashes are input-specific, so "is this value somewhere in the fixture"
# is not the question — a membership test passes a prose copy that quoted
# the absent-media edge vector beside the golden's own inputs.
R="$TMP/prose-wrong-vector"; make_fixture "$R" "$GOLDEN"
python3 - "$R/fixtures/direct-v2/f.json" <<'PY'
import json,sys
p=sys.argv[1]; d=json.load(open(p))
d['vectors'].append({"name":"absent-media","media_type":None,
                     "payload_hex":"68656c6c6f",
                     "sha256":"d75d8054e7c0af3c45744e92a0794fa4b18335adf0267965931064c0636bdb86"})
json.dump(d, open(p,'w'))
PY
mkdir -p "$R/architecture/contracts"
printf 'media_type = "text/plain"\npayload    = UTF8("hello")\nSHA-256    = %s\n' \
    "d75d8054e7c0af3c45744e92a0794fa4b18335adf0267965931064c0636bdb86" \
    > "$R/architecture/contracts/SPEC.md"
[ "$(run_code "$R")" = "1" ] && ok "a neighbouring vector's hash quoted for this golden is caught" \
    || bad "membership in the file must not be sufficient"

# ── an unrelated hash near OTHER inputs is not attributed ────────────────
# One document can list several frozen goldens — ADR-0047 lists four — so
# attribution is by proximity to a vector's own inputs, not by "this file
# mentions them somewhere and also contains a hash".
R="$TMP/prose-neighbour"; make_fixture "$R" "$GOLDEN"
mkdir -p "$R/architecture/adr"
printf 'DirectContentFingerprint\n  media_type = "text/plain"\n  payload = UTF8("hello")\n  SHA-256 = %s\n\nTopic key\n  ChannelId = "general"\n  SHA-256 = %s\n' \
    "$GOLDEN" "1111111111111111111111111111111111111111111111111111111111111111" \
    > "$R/architecture/adr/0047-names.md"
[ "$(run_code "$R")" = "0" ] && ok "a neighbouring golden is not misattributed" || bad "neighbour should not be flagged: $(run "$R")"

# ── the real fixtures verify ─────────────────────────────────────────────
REAL="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel 2>/dev/null)"
if [ -n "$REAL" ]; then
    [ "$(run_code "$REAL")" = "0" ] && ok "the real frozen vectors recompute" || bad "real fixtures fail: $(run "$REAL")"
fi

# ── usage ────────────────────────────────────────────────────────────────
[ "$(run_code "$TMP/nothing-here")" = "2" ] && ok "a missing fixtures/ exits 2" || bad "missing tree should exit 2"
help_out="$(python3 "$CHECK" --help 2>/dev/null)"
[[ "$help_out" == *"frozen conformance vector"* ]] && ok "--help prints the help block" || bad "--help should print help"

printf '\n'
if [ "$fail" -gt 0 ]; then
    printf 'test_verify_fixture_vectors: %d passed, %d FAILED.\n' "$pass" "$fail" >&2
    exit 1
fi
printf 'test_verify_fixture_vectors: OK — all %d assertions passed.\n' "$pass"
