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
  manifest="$(cargo metadata --format-version 1 --locked 2>/dev/null \
    | python3 -c '
import json,sys
meta = json.load(sys.stdin)
for pkg in meta["packages"]:
    if pkg["name"] == "libp2p-gossipsub":
        print(pkg["manifest_path"])
        break
' || true)"

  if [[ -z "$manifest" ]]; then
    report "libp2p-gossipsub is not in the dependency graph — if broadcast was removed, remove this check with it"
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
sed 's;//.*$;;' "$protocol"  > "$stripped_protocol"
sed 's;//.*$;;' "$behaviour" > "$stripped_behaviour"
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
  body="$(awk '/if +verify_signature +&& +!.*verify_signature\(&message\)/{f=1} f{print} f&&/^ *\}$/{exit}' "$protocol")"
  if ! grep -qE 'ValidationError::InvalidSignature' <<<"$body"; then
    report "the failed-signature branch no longer records an InvalidSignature validation error"
  fi
  # A BARE `continue;` or `return;` ONLY. `return <expr>;` yields a value
  # -- a decoded message, an Ok -- which is delivery, not rejection, and
  # a regex that accepted any `return` read `return message;` as a
  # refusal. The rejection here abandons the message; anything that hands
  # one back has to be read by a person, not matched by this.
  if ! grep -qE '^[[:space:]]*(continue|return)[[:space:]]*;' <<<"$body"; then
    report "the failed-signature branch no longer abandons the message — a bare continue/return is gone, so it may now yield one to the behaviour"
  fi
  # SEPARATELY, because the mesh id is a PAIR. One assertion covering
  # both fields can lose half of itself and still fire on a fixture that
  # mutates the other half — which is exactly how the first version of
  # this hid the loss of the sequence-number protection.
  if ! grep -qE 'source: *None' <<<"$body"; then
    report "the failed-signature branch no longer blanks source — a forgery could carry a publisher identity into the cache"
  fi
  if ! grep -qE 'sequence_number: *None' <<<"$body"; then
    report "the failed-signature branch no longer blanks sequence_number — a forgery could carry the other half of a mesh id into the cache"
  fi
fi

# 3. The duplicate cache still lives in the behaviour, downstream of that.
if ! grep -q 'duplicate_cache.insert' "$behaviour"; then
  report "behaviour.rs no longer inserts into duplicate_cache — re-read where suppression happens now"
fi
if grep -q 'duplicate_cache' "$protocol"; then
  report "the duplicate cache is now reachable from the decoder — the separation this check exists for is gone"
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
