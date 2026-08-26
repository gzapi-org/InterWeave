#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/test_check_domain_fns_are_called.sh
#
# Self-test for check_domain_fns_are_called.sh.
#
# The guard's whole value is that it FAILS on a `pub fn` nobody calls, so
# the load-bearing cases here are the positive ones. Case 1 is the actual
# Stage 6 defect — `authorize_outbound`, written and tested and never
# called — reproduced in miniature.
#
# The exemption cases matter just as much, because an allow-list is how a
# check like this dies: an expired deadline, an entry for a function that
# is called after all, and an entry naming nothing must all be failures,
# or the file quietly grows into a blanket exemption.
#
# Exit codes:
#   0  all assertions passed
#   1  one or more failed

set -uo pipefail

SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
UNDER_TEST="$SCRIPT_DIR/check_domain_fns_are_called.sh"
[[ -f "$UNDER_TEST" ]] || { echo "test: $UNDER_TEST not found" >&2; exit 1; }

failures=0
SANDBOX=""
cleanup() { [[ -n "$SANDBOX" && -d "$SANDBOX" ]] && rm -rf "$SANDBOX"; }
trap cleanup EXIT

pass() { echo "  ✓ $1"; }
fail() { echo "  ✗ $1" >&2; printf '%s\n' "${2:-}" | sed 's/^/      /' >&2
         failures=$((failures + 1)); }

# A throwaway repository shaped like the real one: a domain crate under
# `crates/transport/runtime/`, a backend that may or may not call into
# it, a manifest carrying the open stage, and an exemption file. The
# guard runs against `git ls-files` exactly as it does for real.
#   $1 domain source   $2 backend source   $3 exemption file   $4 stage
run_against() {
    SANDBOX="$(mktemp -d)"
    mkdir -p "$SANDBOX/tools/checks" \
             "$SANDBOX/crates/transport/runtime/src" \
             "$SANDBOX/crates/transport/libp2p/src"
    cp "$UNDER_TEST" "$SANDBOX/tools/checks/"
    printf '%s\n' "$1" > "$SANDBOX/crates/transport/runtime/src/lib.rs"
    printf '%s\n' "$2" > "$SANDBOX/crates/transport/libp2p/src/lib.rs"
    printf '%s\n' "$3" > "$SANDBOX/tools/checks/domain_fn_exempt.txt"
    printf 'status = "%s"\n' "${4:-stage-6-direct-v2}" > "$SANDBOX/Cargo.toml"
    git -C "$SANDBOX" init -q
    git -C "$SANDBOX" add -A
    RUN_OUT="$(cd "$SANDBOX" && bash tools/checks/check_domain_fns_are_called.sh 2>&1)"
    RUN_RC=$?
    rm -rf "$SANDBOX"; SANDBOX=""
}

assert_rc() {
    if [[ "$RUN_RC" -eq "$2" ]]; then pass "$1"
    else fail "$1 — expected exit $2, got $RUN_RC" "$RUN_OUT"; fi
}
assert_says() {
    if grep -qF -- "$2" <<<"$RUN_OUT"; then pass "$1"
    else fail "$1 — output did not mention '$2'" "$RUN_OUT"; fi
}

CALLED='pub fn admit(x: u8) -> u8 { x }'
UNCALLED='pub fn authorize_outbound(x: u8) -> u8 { x }'
BACKEND_CALLS='fn go() { let _ = admit(1); }'
BACKEND_IDLE='fn go() {}'

echo "check_domain_fns_are_called self-test"

# --- the defect this guard exists for --------------------------------
run_against "$UNCALLED" "$BACKEND_IDLE" "" ""
assert_rc   "a pub fn with no caller anywhere fails" 1
assert_says "  and it names the function" 'authorize_outbound'

run_against "$CALLED" "$BACKEND_CALLS" "" ""
assert_rc   "a pub fn the backend calls passes" 0

# A caller in the SAME file is not a caller: `authorize_outbound` had
# unit tests beside it and that is precisely why it looked covered.
run_against "$UNCALLED
mod t { use super::*; fn probe() { let _ = authorize_outbound(1); } }" "$BACKEND_IDLE" "" ""
assert_rc   "a caller in the defining file alone does not count" 1

# --- scope ------------------------------------------------------------
run_against 'pub(crate) fn narrow(x: u8) -> u8 { x }' "$BACKEND_IDLE" "" ""
assert_rc   "pub(crate) is out of scope" 0

run_against 'fn private(x: u8) -> u8 { x }' "$BACKEND_IDLE" "" ""
assert_rc   "a private fn is out of scope" 0

# --- exemptions are deadlines, not a snooze button --------------------
run_against "$UNCALLED" "$BACKEND_IDLE" \
    'authorize_outbound stage-9 waits for the stage that wires it' ""
assert_rc   "an exemption whose stage is ahead passes" 0

run_against "$UNCALLED" "$BACKEND_IDLE" \
    'authorize_outbound stage-3 should have been wired long ago' ""
assert_rc   "an exemption whose stage has passed fails" 1
assert_says "  and it says the deadline passed" 'deadline passed'

run_against "$UNCALLED" "$BACKEND_IDLE" \
    'authorize_outbound stage-6 the open stage is not yet past' ""
assert_rc   "an exemption for the OPEN stage still passes" 0

run_against "$CALLED" "$BACKEND_CALLS" \
    'admit stage-9 exempt but actually called' ""
assert_rc   "an exemption for a function that IS called fails" 1
assert_says "  and it says to drop the entry" 'drop the entry'

run_against "$CALLED" "$BACKEND_CALLS" \
    'ghost_fn stage-9 names nothing' ""
assert_rc   "an exemption naming no function fails" 1
assert_says "  and it calls the entry stale" 'stale entry'

# --- a method is qualified by its type -------------------------------
#
# The reviewer's case on PR #41: matching a bare name lets every `new` in
# the tree vouch for every other one. `ObservedCandidates::new` was
# referenced nowhere outside its own file and passed anyway, because
# sixteen unrelated `new` methods existed.
ALPHA_NEW='impl Alpha {
    pub fn new() -> u8 { 0 }
}'
run_against "$ALPHA_NEW" 'fn go() { let _ = Beta::new(); }' "" ""
assert_rc   "a same-named method on an unrelated type is not a caller" 1
assert_says "  and it reports the qualified name" 'Alpha::new'

run_against "$ALPHA_NEW" 'fn go() { let _ = Alpha::new(); }' "" ""
assert_rc   "naming the type makes it a caller" 0

# But requiring the type NAME alone would report a function that is
# called twice: `refusal.to_wire()` never writes `Refusal`.
run_against 'impl Refusal {
    pub fn to_wire(&self) -> u8 { 0 }
}' 'fn go(r: X) { let _ = r.to_wire(); }' "" ""
assert_rc   "a call in method position counts without naming the type" 0

# --- exemptions are qualified too ------------------------------------
run_against "$ALPHA_NEW" "$BACKEND_IDLE" 'new stage-9 a bare name must not cover a method' ""
assert_rc   "a bare exemption does not cover a qualified method" 1
assert_says "  the qualified name is still reported" 'Alpha::new'

run_against "$ALPHA_NEW" "$BACKEND_IDLE" 'Alpha::new stage-9 qualified and ahead' ""
assert_rc   "a qualified exemption covers it" 0

# --- a malformed exemption file is a hard error, not a pass -----------
run_against "$UNCALLED" "$BACKEND_IDLE" 'authorize_outbound no reason and no stage' ""
assert_rc   "an exemption without a stage-N deadline is exit 2" 2

run_against "$UNCALLED" "$BACKEND_IDLE" 'authorize_outbound stage-9' ""
assert_rc   "an exemption with a deadline but no reason is exit 2" 2

run_against "$UNCALLED" "$BACKEND_IDLE" '' 'no-stage-here'
assert_rc   "an unreadable open stage is exit 2" 2

# --- comments and blank lines are not entries -------------------------
run_against "$UNCALLED" "$BACKEND_IDLE" \
    '# a comment

authorize_outbound stage-9 real entry below a comment' ""
assert_rc   "comments and blank lines are skipped" 0

echo
if (( failures > 0 )); then
    echo "check_domain_fns_are_called self-test: $failures failed" >&2
    exit 1
fi
echo "check_domain_fns_are_called self-test: all passed"
