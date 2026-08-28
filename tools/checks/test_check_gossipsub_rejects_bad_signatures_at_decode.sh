#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/test_check_gossipsub_rejects_bad_signatures_at_decode.sh
#
# Self-test for check_gossipsub_rejects_bad_signatures_at_decode.sh.
#
# EVERY CASE RUNS THE REAL GUARD, pointed at a stand-in source tree
# through INTERWEAVE_GOSSIPSUB_SRC. The first version of this file
# reimplemented the guard's assertions in a local `probe` and tested
# that instead — so weakening or deleting an assertion in the guard left
# every case green, which is precisely the failure the guard itself
# exists to prevent, one level up. A self-test that cannot fail when its
# subject is weakened is not a self-test.
#
# The stand-ins are fixtures rather than the registry checkout, so the
# negative cases cannot be made to pass by editing someone else's source,
# and the suite does not depend on a populated cargo cache.
#
# Exit codes:
#   0  all assertions passed
#   1  one or more failed

set -uo pipefail

SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
GUARD="$SCRIPT_DIR/check_gossipsub_rejects_bad_signatures_at_decode.sh"

failures=0
ok()  { echo "  ✓ $*"; }
bad() { echo "  ✗ $*"; failures=$((failures + 1)); }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/src"

# A stand-in with the shape the guard is written against.
# The stand-in is the REAL branch's shape, since the guard now compares a
# digest of it. Each negative case mutates one thing and must fail.
write_good() {
  cat > "$work/src/protocol.rs" <<'RS'
    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
            match self.validation_mode {
                ValidationMode::Strict => {
                    verify_signature = true;
                }
                ValidationMode::Permissive => {}
            }

            if verify_signature && !GossipsubCodec::verify_signature(&message) {
                tracing::warn!("Invalid signature for received message");
                let message = RawMessage {
                    source: None,
                    sequence_number: None,
                    validated: false,
                };
                invalid_messages.push((message, ValidationError::InvalidSignature));
                continue;
            }
            Ok(Some(rpc))
    }
RS
  cat > "$work/src/behaviour.rs" <<'RS'
        if !self.duplicate_cache.insert(msg_id.clone()) { return; }

    fn handle_invalid_message(
        &mut self,
        propagation_source: &PeerId,
        topic: &TopicHash,
        reject_reason: RejectReason,
    ) {
        if let PeerScoreState::Active(peer_score) = &mut self.peer_score {
            peer_score.reject_message(propagation_source, topic, reject_reason);
        }
    }
RS
  # Pin the guard's expectation to THIS fixture, so every case below
  # exercises the real comparison rather than a copy of it.
  export INTERWEAVE_REVIEWED_PROTOCOL_SHA256="$(
    sed 's;//.*$;;' "$work/src/protocol.rs" \
      | sed 's/[[:space:]]\+/ /g;s/^ //;s/ $//' | grep -v '^$' | sha256sum | cut -d' ' -f1
  )"
  export INTERWEAVE_REVIEWED_BEHAVIOUR_SHA256="$(
    sed 's;//.*$;;' "$work/src/behaviour.rs" \
      | sed 's/[[:space:]]\+/ /g;s/^ //;s/ $//' | grep -v '^$' | sha256sum | cut -d' ' -f1
  )"
}

run_guard() { INTERWEAVE_GOSSIPSUB_SRC="$work/src" bash "$GUARD" >/dev/null 2>&1; }

mutate() {  # <description> <python-snippet>
  write_good
  python3 - "$work/src/protocol.rs" <<PY
import io,sys
p = sys.argv[1]
s = io.open(p).read()
$2
io.open(p, "w").write(s)
PY
  if run_guard; then bad "$1"; else ok "caught it"; fi
}

echo "gossipsub signature guard: the shape it is written against"
write_good
if run_guard; then ok "accepts the reviewed branch"; else bad "rejects the branch it pinned"; fi

echo "gossipsub signature guard: Strict stops verifying signatures"
write_good
sed -i 's/                    verify_signature = true;//' "$work/src/protocol.rs"
if run_guard; then bad "did not notice Strict no longer sets verify_signature"; else ok "caught it"; fi

echo "gossipsub signature guard: the branch abandons via a nested conditional"
mutate "read a conditional continue as unconditional abandonment" \
  's = s.replace("                continue;", "                if false { continue; }", 1)'

echo "gossipsub signature guard: the branch returns a value instead of abandoning"
mutate "read a returned message as a rejection" \
  's = s.replace("                continue;", "                return message;", 1)'

echo "gossipsub signature guard: the rejected message keeps its source"
mutate "did not notice a forgery may carry a publisher identity past the decoder" \
  's = s.replace("source: None,", "source: Some(peer),", 1)'

echo "gossipsub signature guard: the rejected message keeps its sequence number"
mutate "did not notice a forgery may carry the other half of a mesh id past the decoder" \
  's = s.replace("sequence_number: None,", "sequence_number: Some(seq),", 1)'

echo "gossipsub signature guard: the branch stops recording an invalid signature"
mutate "did not notice the validation error changed" \
  's = s.replace("ValidationError::InvalidSignature", "ValidationError::SomethingElse", 1)'

echo "gossipsub signature guard: the condition survives only as a line comment"
mutate "matched a commented-out condition while the live one was disabled" \
  's = s.replace("            if verify_signature", "            // if verify_signature", 1).replace("!GossipsubCodec::verify_signature(&message) {", "!GossipsubCodec::verify_signature(&message) {\n            if false {", 1)'

echo "gossipsub signature guard: the branch survives only inside a block comment"
mutate "matched a branch that a block comment had disabled" \
  's = s.replace("            if verify_signature && !GossipsubCodec::verify_signature(&message) {", "            /* if verify_signature && !GossipsubCodec::verify_signature(&message) { */\n            if false {", 1)'

echo "gossipsub signature guard: a bypass inserted above the reviewed branch"
mutate "left a pre-verification delivery path invisible" \
  's = s.replace("            if verify_signature", "            if bypass { decoded.push(message.clone()); continue; }\n            if verify_signature", 1)'

echo "gossipsub signature guard: a comment-only edit is tolerated"
write_good
sed -i 's|tracing::warn!("Invalid signature for received message");|// reworded\n                tracing::warn!("Invalid signature for received message");|' "$work/src/protocol.rs"
if run_guard; then ok "comment churn does not fail the check"; else bad "a comment-only change failed it"; fi

echo "gossipsub signature guard: the duplicate cache moves into the decoder"
write_good
echo 'self.duplicate_cache.insert(id);' >> "$work/src/protocol.rs"
if run_guard; then bad "did not notice the cache became reachable before validation"; else ok "caught it"; fi

echo "gossipsub signature guard: invalid messages start reaching the cache"
write_good
python3 - "$work/src/behaviour.rs" <<'PY'
import io,sys
p = sys.argv[1]
s = io.open(p).read()
s = s.replace("            peer_score.reject_message(propagation_source, topic, reject_reason);",
              "            peer_score.reject_message(propagation_source, topic, reject_reason);\n        }\n        if true {\n            self.duplicate_cache.insert(msg_id.clone());", 1)
io.open(p, "w").write(s)
PY
if run_guard; then
  bad "an invalid-message cache path was added and the separation check stayed green"
else
  ok "caught it"
fi

echo "gossipsub signature guard: a new dispatcher caches invalid messages"
write_good
python3 - "$work/src/behaviour.rs" <<'PY'
import io,sys
p = sys.argv[1]
s = io.open(p).read()
s = "    fn dispatch_invalid_messages(&mut self, msgs: Vec<RawMessage>) {\n        for m in msgs { self.duplicate_cache.insert(m.id.clone()); }\n    }\n\n" + s
io.open(p, "w").write(s)
PY
if run_guard; then
  bad "a new invalid-message dispatcher reached the cache unnoticed"
else
  ok "caught it"
fi

echo "gossipsub signature guard: the cache disappears from the behaviour"
write_good
sed -i 's/duplicate_cache.insert/some_other_cache.insert/' "$work/src/behaviour.rs"
if run_guard; then bad "did not notice the cache it reasons about is gone"; else ok "caught it"; fi

unset INTERWEAVE_REVIEWED_PROTOCOL_SHA256 INTERWEAVE_REVIEWED_BEHAVIOUR_SHA256
echo "gossipsub signature guard: the dependency is not the reviewed version"
if INTERWEAVE_REVIEWED_GOSSIPSUB_VERSION="0.0.0-not-a-release" bash "$GUARD" >/dev/null 2>&1; then
  bad "hashed a version it had not reviewed"
else
  ok "refuses a version it has not reviewed"
fi

echo "gossipsub signature guard: it runs against the real crate"
if bash "$GUARD" >/dev/null 2>&1; then
  ok "passes on the pinned dependency"
else
  bad "fails on the pinned dependency — the guarantee moved, or the guard is wrong"
fi

echo ""
if [[ $failures -eq 0 ]]; then
  echo "test_check_gossipsub_rejects_bad_signatures_at_decode: OK — all assertions passed."
  exit 0
fi
echo "test_check_gossipsub_rejects_bad_signatures_at_decode: $failures assertion(s) failed." >&2
exit 1
