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
write_good() {
  cat > "$work/src/protocol.rs" <<'RS'
            match self.validation_mode {
                ValidationMode::Strict => {
                    verify_signature = true;
                    verify_sequence_no = true;
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
RS
  cat > "$work/src/behaviour.rs" <<'RS'
        if !self.duplicate_cache.insert(msg_id.clone()) {
            return;
        }
RS
}

run_guard() { INTERWEAVE_GOSSIPSUB_SRC="$work/src" bash "$GUARD" >/dev/null 2>&1; }

echo "gossipsub signature guard: the shape it is written against"
write_good
if run_guard; then ok "accepts the pinned layout"; else bad "rejects the layout it was written against"; fi

echo "gossipsub signature guard: Strict stops verifying signatures"
write_good
sed -i 's/                    verify_signature = true;//' "$work/src/protocol.rs"
if run_guard; then bad "did not notice Strict no longer sets verify_signature"; else ok "caught it"; fi

echo "gossipsub signature guard: the condition survives but the rejection is gone"
write_good
sed -i 's/^                continue;$//' "$work/src/protocol.rs"
if run_guard; then
  bad "did not notice the branch no longer terminates decoding — the condition-only case"
else
  ok "caught it"
fi

echo "gossipsub signature guard: the branch stops recording an invalid signature"
write_good
sed -i 's/ValidationError::InvalidSignature/ValidationError::SomethingElse/' "$work/src/protocol.rs"
if run_guard; then bad "did not notice the validation error changed"; else ok "caught it"; fi

echo "gossipsub signature guard: the rejected message keeps its source and sequence"
write_good
sed -i 's/                    source: None,/                    source: Some(peer),/' "$work/src/protocol.rs"
if run_guard; then
  bad "did not notice a forgery may now carry an id past the decoder"
else
  ok "caught it"
fi

echo "gossipsub signature guard: the decoder stops testing the signature at all"
write_good
sed -i 's/^            if verify_signature && !GossipsubCodec::verify_signature(&message) {/            if false {/' "$work/src/protocol.rs"
if run_guard; then bad "did not notice the decode-time test is gone"; else ok "caught it"; fi

echo "gossipsub signature guard: the duplicate cache moves into the decoder"
write_good
echo 'self.duplicate_cache.insert(id);' >> "$work/src/protocol.rs"
if run_guard; then bad "did not notice the cache became reachable before validation"; else ok "caught it"; fi

echo "gossipsub signature guard: the cache disappears from the behaviour"
write_good
sed -i 's/duplicate_cache.insert/some_other_cache.insert/' "$work/src/behaviour.rs"
if run_guard; then bad "did not notice the cache it reasons about is gone"; else ok "caught it"; fi

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
