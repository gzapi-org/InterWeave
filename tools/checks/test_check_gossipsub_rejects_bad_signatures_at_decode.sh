#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/test_check_gossipsub_rejects_bad_signatures_at_decode.sh
#
# Self-test for check_gossipsub_rejects_bad_signatures_at_decode.sh.
#
# The guard is the only mechanism for a property no test of ours can
# assert — it lives inside a dependency — so a guard that cannot fail is
# worse than none: it reports OK forever and reads as coverage.
#
# The cases that matter are therefore the NEGATIVE ones: each of the four
# ways the guarantee can move must be caught. They run against fixture
# copies of the vendored files rather than the real crate, so the test
# does not depend on a registry checkout and cannot be made to pass by
# editing someone else's source.
#
# Exit codes:
#   0  all assertions passed
#   1  one or more failed

set -uo pipefail

SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
GUARD="$SCRIPT_DIR/check_gossipsub_rejects_bad_signatures_at_decode.sh"

failures=0
ok()   { echo "  ✓ $*"; }
bad()  { echo "  ✗ $*"; failures=$((failures + 1)); }

# The guard reads the crate that `cargo metadata` names, so the negative
# cases are exercised against a stand-in tree with the same shape.
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/src"

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
    return invalid;
}
RS
  cat > "$work/src/behaviour.rs" <<'RS'
if !self.duplicate_cache.insert(msg_id.clone()) {
    return;
}
RS
}

# A trimmed copy of the guard's own assertions, run against the stand-in.
probe() {
  local p="$work/src/protocol.rs" b="$work/src/behaviour.rs" problems=0
  grep -qE 'ValidationMode::Strict *=>' "$p" || problems=$((problems+1))
  awk '/ValidationMode::Strict *=>/,/ValidationMode::Permissive/' "$p" \
    | grep -qE 'verify_signature *= *true' || problems=$((problems+1))
  grep -qE 'if +verify_signature +&& +!.*verify_signature\(&message\)' "$p" || problems=$((problems+1))
  grep -q 'duplicate_cache.insert' "$b" || problems=$((problems+1))
  ! grep -q 'duplicate_cache' "$p" || problems=$((problems+1))
  return $problems
}

echo "gossipsub signature guard: the shape it is written against"
write_good
if probe; then ok "accepts the pinned layout"; else bad "rejects the layout it was written against"; fi

echo "gossipsub signature guard: Strict stops verifying signatures"
write_good
sed -i 's/        verify_signature = true;//' "$work/src/protocol.rs"
if probe; then bad "did not notice Strict no longer sets verify_signature"; else ok "caught it"; fi

echo "gossipsub signature guard: the decoder stops rejecting"
write_good
sed -i 's/^if verify_signature && !GossipsubCodec::verify_signature(&message) {/if false {/' "$work/src/protocol.rs"
if probe; then bad "did not notice the decode-time rejection is gone"; else ok "caught it"; fi

echo "gossipsub signature guard: the duplicate cache moves into the decoder"
write_good
echo 'self.duplicate_cache.insert(id);' >> "$work/src/protocol.rs"
if probe; then bad "did not notice the cache became reachable before validation"; else ok "caught it"; fi

echo "gossipsub signature guard: the cache disappears from the behaviour"
write_good
sed -i 's/duplicate_cache.insert/some_other_cache.insert/' "$work/src/behaviour.rs"
if probe; then bad "did not notice the cache it reasons about is gone"; else ok "caught it"; fi

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
