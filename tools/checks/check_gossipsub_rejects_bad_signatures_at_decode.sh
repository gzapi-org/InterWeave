#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/check_gossipsub_rejects_bad_signatures_at_decode.sh
#
# >>> help
# Does the pinned GossipSub still reject invalid signatures during DECODE,
# before a message can reach the duplicate cache?
#
# PUBSUB.md requires that invalid-signature traffic cannot poison the
# duplicate cache against later authentic traffic. Stage 7 proves the
# observable half over the wire: forged traffic bearing a publisher's
# identity does not stop that publisher's real message arriving. It cannot
# prove the mechanism, because the collision that would test it is not
# constructible from outside — `sequence_number` is assigned inside the
# backend, so no publisher, honest or otherwise, chooses the `(source,
# sequence)` pair a forgery would need to match.
#
# What makes the property hold is where the check happens. In
# `libp2p-gossipsub`, signature verification runs in the CODEC's decoder
# (`protocol.rs`), not in the behaviour:
#
#     ValidationMode::Strict => { verify_signature = true; ... }
#     if verify_signature && !GossipsubCodec::verify_signature(&message) {
#
# A message that fails it is turned into an invalid-message event with
# `source: None` and `sequence_number: None`, and `handle_received_message`
# — which owns `duplicate_cache` — is never reached. So a forged message
# cannot occupy a cache entry under ANY id, which is stronger than an
# ordering within the behaviour and is why the wire test cannot see it.
#
# WHY A BESPOKE CHECK. This is a property of a dependency's internals, so
# no test of ours can assert it and no lockfile pin can describe it: the
# version could be bumped, the code reorganised, and every check stay
# green while the guarantee the stage rests on quietly moved. The review
# that raised this named exactly that risk — a backend upgrade changing
# validation or cache ordering.
#
# It fails on an upgrade that moves or renames these, which is the point:
# the guarantee must be re-established by reading the new code, not
# assumed to have survived.
# <<< help

set -euo pipefail

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  sed -n '/^# >>> help$/,/^# <<< help$/p' "$0" | sed '1d;$d' | sed 's/^# \{0,1\}//'
  exit 0
fi

cd "$(dirname "$0")/../.."

# The normalised `Decoder::decode` of the version below, as read by a
# person. The WHOLE function, not just the failed-signature branch:
# fingerprinting the branch alone said nothing about the path to it, so a
# bypass inserted above it left the digest matching. What the stage rests
# on is that EVERY decoded message reaches the rejection, which is a
# property of the path, so the path is what is pinned.
#
# Recomputed by the failure message's own instructions; changing it
# without reading the new function defeats the entire check.
REVIEWED_GOSSIPSUB_VERSION="${INTERWEAVE_REVIEWED_GOSSIPSUB_VERSION:-0.49.5}"
# Overridable ONLY so the self-test can pin its own fixtures, for the same
# reason INTERWEAVE_GOSSIPSUB_SRC exists: a self-test that reimplements
# the comparison cannot fail when the real one is weakened.
REVIEWED_PROTOCOL_SHA256="${INTERWEAVE_REVIEWED_PROTOCOL_SHA256:-5a2fe62c6b1d89a51299c9f78024cb196080cb84f820d47d02f102493b9790f2}"
REVIEWED_BEHAVIOUR_SHA256="${INTERWEAVE_REVIEWED_BEHAVIOUR_SHA256:-32b106d723342d35612168b63359a600c24f0838f22ef62fcfe5ce2ad37464fb}"

problems=0
report() {
  echo "check_gossipsub_rejects_bad_signatures_at_decode: $*" >&2
  problems=$((problems + 1))
}

# The source root, overridable ONLY so the self-test can run this exact
# script against mutated stand-ins. Without it the self-test would have to
# reimplement these assertions, and a reimplementation cannot fail when the
# real ones are weakened -- which is the failure mode this whole guard
# exists to prevent, one level up.
if [[ -n "${INTERWEAVE_GOSSIPSUB_SRC:-}" ]]; then
  src="$INTERWEAVE_GOSSIPSUB_SRC"
else
  # EXACTLY ONE, AND THE REVIEWED VERSION. Taking the first match and
  # never checking its version -- which is what this did, with
  # REVIEWED_GOSSIPSUB_VERSION declared and unused -- means a graph
  # holding both the reviewed release and a newer one could hash the
  # DORMANT copy and report on code nothing runs. Ambiguity here is not
  # something to resolve cleverly; it is something to refuse.
  selected="$(cargo metadata --format-version 1 --locked 2>/dev/null \
    | python3 -c '
import json,sys
meta = json.load(sys.stdin)
found = [p for p in meta["packages"] if p["name"] == "libp2p-gossipsub"]
for p in found:
    print(p["version"], p["manifest_path"])
' || true)"

  count="$(printf '%s' "$selected" | grep -c . || true)"
  if [[ "$count" -eq 0 ]]; then
    report "libp2p-gossipsub is not in the dependency graph — if broadcast was removed, remove this check with it"
    exit 1
  fi
  if [[ "$count" -gt 1 ]]; then
    report "the graph holds $count libp2p-gossipsub versions:"
    printf '%s\n' "$selected" | sed 's/^/    /' >&2
    report "which one serves the transport is not a question this check can answer — resolve the duplicate, or pin the guard to the one that does"
    exit 1
  fi

  found_version="$(printf '%s' "$selected" | cut -d' ' -f1)"
  manifest="$(printf '%s' "$selected" | cut -d' ' -f2-)"
  if [[ "$found_version" != "$REVIEWED_GOSSIPSUB_VERSION" ]]; then
    report "libp2p-gossipsub is $found_version, reviewed $REVIEWED_GOSSIPSUB_VERSION — read the new source and re-establish the guarantee before moving the pins"
    exit 1
  fi
  src="$(dirname "$manifest")/src"
fi

protocol="$src/protocol.rs"
behaviour="$src/behaviour.rs"

for f in "$protocol" "$behaviour"; do
  [[ -r "$f" ]] || report "cannot read $f"
done
[[ $problems -eq 0 ]] || exit 1

# EVERY ASSERTION READS CODE, NEVER COMMENTS. A revision that replaced
# the live condition with `if false` while leaving the original in a
# comment above it satisfied every check below: the greps matched the
# comment and the body they then examined was the real one, still intact
# and now unreachable. Line comments are stripped once, here, so no
# assertion can be fooled by prose that merely looks like the code.
stripped_protocol="$(mktemp)"
stripped_behaviour="$(mktemp)"
trap 'rm -f "$stripped_protocol" "$stripped_behaviour"' EXIT
strip_comments() {
  # Block comments first (they may span lines), then line comments.
  python3 -c '
import re,sys
src = open(sys.argv[1]).read()
src = re.sub(r"/\*.*?\*/", "", src, flags=re.S)
src = re.sub(r"//[^\n]*", "", src)
sys.stdout.write(src)
' "$1"
}
strip_comments "$protocol"  > "$stripped_protocol"
strip_comments "$behaviour" > "$stripped_behaviour"
protocol="$stripped_protocol"
behaviour="$stripped_behaviour"

# 1. Strict still asks for signature verification.
if ! grep -qE 'ValidationMode::Strict *=>' "$protocol"; then
  report "protocol.rs no longer matches on ValidationMode::Strict"
elif ! awk '/ValidationMode::Strict *=>/,/ValidationMode::Permissive/' "$protocol" \
     | grep -qE 'verify_signature *= *true'; then
  report "ValidationMode::Strict no longer sets verify_signature — a forged message may now reach the behaviour"
fi

# 2. The decoder still refuses a message whose signature does not verify —
#    and the REJECTION ITSELF, not merely the condition. A refactor that
#    keeps the test for logging and drops the early exit would leave a
#    condition-only assertion green while invalid messages walked on to
#    the behaviour.
if ! grep -qE 'if +verify_signature +&& +!.*verify_signature\(&message\)' "$protocol"; then
  report "protocol.rs no longer tests the signature during decode"
else
  # THE FILES ARE FINGERPRINTED, NOT DESCRIBED.
  #
  # Nine rounds of review found nine ways to satisfy a description of
  # this guarantee while defeating it. Five were regexes matching a token
  # rather than a meaning. The last four were all the same shape: whatever
  # scope was pinned, the bypass went just outside it -- the branch, then
  # the path to the branch, then the invalid-message handler, then the
  # code that dispatches to that handler. Each widening was correct and
  # left a boundary for the next one.
  #
  # There is no natural boundary, so the file is the boundary. Both files
  # the guarantee depends on are normalised -- comments stripped,
  # whitespace collapsed -- and hashed. The check claims exactly one
  # thing and verifies it completely: THIS CODE HAS NOT CHANGED.
  #
  # Brittle by construction, and it costs nothing: the version is pinned,
  # so these files do not change between deliberate bumps. When one
  # happens, the check fails and the guarantee is re-established by
  # reading -- which is what it was always asking for.
  actual_protocol="$(sed 's/[[:space:]]\+/ /g;s/^ //;s/ $//' "$protocol" | grep -v '^$' | sha256sum | cut -d' ' -f1)"
  if [[ "$actual_protocol" != "$REVIEWED_PROTOCOL_SHA256" ]]; then
    report "protocol.rs has changed (sha256 $actual_protocol, reviewed $REVIEWED_PROTOCOL_SHA256)"
  fi

fi

# 3. The duplicate cache still lives in the behaviour, downstream of that.
if ! grep -q 'duplicate_cache.insert' "$behaviour"; then
  report "behaviour.rs no longer inserts into duplicate_cache — re-read where suppression happens now"
fi
if grep -q 'duplicate_cache' "$protocol"; then
  report "the duplicate cache is now reachable from the decoder — the separation this check exists for is gone"
fi

# 4. And the whole of behaviour.rs, for the same reason: pinning the
#    invalid-message HANDLER said nothing about the code that dispatches
#    to it, which was the ninth finding and the third of that shape.
actual_behaviour="$(sed 's/[[:space:]]\+/ /g;s/^ //;s/ $//' "$behaviour" | grep -v '^$' | sha256sum | cut -d' ' -f1)"
if [[ "$actual_behaviour" != "$REVIEWED_BEHAVIOUR_SHA256" ]]; then
  report "behaviour.rs has changed (sha256 $actual_behaviour, reviewed $REVIEWED_BEHAVIOUR_SHA256)"
fi


if [[ $problems -gt 0 ]]; then
  echo "" >&2
  echo "The pinned GossipSub's signature handling moved. Stage 7's cache-poisoning" >&2
  echo "clause rests on invalid signatures being refused during DECODE, before the" >&2
  echo "duplicate cache exists to poison. Re-read the new code and re-establish it" >&2
  echo "deliberately; do not adjust this check to match." >&2
  exit 1
fi

echo "check_gossipsub_rejects_bad_signatures_at_decode: OK — invalid signatures are refused in the decoder, upstream of the duplicate cache."
