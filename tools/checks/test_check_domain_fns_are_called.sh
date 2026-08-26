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
# Names the type without calling the method: the type is wired, so its
# methods are policed individually rather than collapsed into one
# type-level finding.
BACKEND_WIRES_ALPHA='fn go(_a: &Alpha) {}'

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
#[cfg(test)]
mod t { use super::*; fn probe() { let _ = authorize_outbound(1); } }" "$BACKEND_IDLE" "" ""
assert_rc   "a unit test beside it does not count as a caller" 1

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
# NOT asserted here: that a file naming both `Alpha` and some other
# type's `new()` fails to vouch for `Alpha::new`. It does vouch, and no
# textual rule can separate the two. The guard documents that limit
# rather than carrying a test that would enshrine the blind spot as a
# feature.

run_against "$ALPHA_NEW" 'fn go() { let _ = Alpha::new(); }' "" ""
assert_rc   "naming the type makes it a caller" 0

# There is NO implicit escape for a method call. A bare `.<name>(` let
# unrelated `.len(` calls vouch for `OfferedAddresses::len`, and matching
# the receiver name reported seven genuinely-called functions as uncalled.
run_against 'impl Refusal {
    pub fn to_wire(&self) -> u8 { 0 }
}' 'fn go(r: X) { let _ = r.to_wire(); }' "" ""
assert_rc   "a bare method call does not vouch without the type" 1

# Two declarations of one name in a file are not uses of each other.
# `as_slice` is declared on both OfferedAddresses and ObservedCandidates
# in the Kademlia port; each counted the other's declaration as its own
# caller and passed with no caller anywhere.
run_against 'impl Alpha {
    pub fn as_slice(&self) -> u8 { 0 }
}
impl Beta {
    pub fn as_slice(&self) -> u8 { 0 }
}' 'fn go(_a: &Alpha, _b: &Beta) {}' "" ""
assert_rc   "two same-named declarations do not vouch for each other" 1
assert_says "  and both are reported" 'Alpha::as_slice'

# --- a comment is not a caller ---------------------------------------
#
# Prose vouched for a function once: `Refusal::to_wire` passed only
# because the conformance matrix's doc comment named both `to_wire` and
# `Refusal`, so a paragraph ABOUT the check was what made it green.
run_against "$ALPHA_NEW" '// Alpha::new is described here but never called.
fn go(_a: &Alpha) {}' "" ""
assert_rc   "a comment naming both the type and the method is not a caller" 1

# --- a test is not a caller ------------------------------------------
#
# The whole point. `authorize_outbound` had unit tests and no production
# caller, so a guard that counted tests would have passed the P1 it
# exists to catch.
run_against "$UNCALLED" '#[cfg(test)]
mod t { fn probe() { let _ = authorize_outbound(1); } }' "" ""
assert_rc   "a caller inside a #[cfg(test)] module elsewhere is not a caller" 1

# A delegating wrapper does not call itself. `OfferedAddresses::len` is
# `self.0.len()`, and counting its own body as a use made every uncalled
# wrapper over a same-named inner method invisible.
run_against 'impl Alpha {
    pub fn len(&self) -> usize { self.0.len() }
}' "$BACKEND_WIRES_ALPHA" "" ""
assert_rc   "a wrapper delegating to a same-named method is not its own caller" 1

# --- `call` exemptions are verified, not asserted ---------------------
run_against 'impl Refusal {
    pub fn to_wire(&self) -> u8 { 0 }
}' 'fn go(r: &Refusal) { let _ = refusal.to_wire(); }' 'Refusal::to_wire call refusal.to_wire(' ""
assert_rc   "a call exemption naming a real call expression passes" 0

run_against 'impl Refusal {
    pub fn to_wire(&self) -> u8 { 0 }
}' 'fn go(_r: &Refusal) {}' 'Refusal::to_wire call refusal.to_wire(' ""
assert_rc   "a call exemption whose call does not exist fails" 1
assert_says "  and it says no production source contains it" 'no PRODUCTION source contains that call'

run_against 'impl Refusal {
    pub fn to_wire(&self) -> u8 { 0 }
}' 'fn go(_r: &Refusal) {}' 'Refusal::to_wire call' ""
assert_rc   "a call exemption with no expression is exit 2" 2

# --- `#[cfg(test)]` marks an ITEM, not the rest of the file -----------
#
# Truncating at the first occurrence discarded every production caller
# below it. `connection_manager.rs` attributes a `thread_local!`
# two-thirds of the way up, so the real call to
# `record_address_failure` vanished and the ledger deferred an
# already-called method to a later stage.
run_against 'impl Alpha {
    pub fn probe(&self) -> u8 { 0 }
}' '#[cfg(test)]
const ONLY_IN_TESTS: u8 = 1;

fn go(a: &Alpha) { let _ = a.probe(); }' "" ""
assert_rc   "production after an attributed item is still read" 0

# The terminal test module must still be dropped, or a unit test beside
# the function vouches for it.
run_against 'impl Alpha {
    pub fn probe(&self) -> u8 { 0 }
}' 'fn go(_a: &Alpha) {}
#[cfg(test)]
mod t { fn probe_it(a: &Alpha) { let _ = a.probe(); } }' "" ""
assert_rc   "a caller inside an attributed module is still not a caller" 1

# --- an unwired type is one finding, not one per method ---------------
run_against 'impl Ghost {
    pub fn new() -> Self { Ghost }
    pub fn len(&self) -> usize { 0 }
    pub fn is_empty(&self) -> bool { true }
}' "$BACKEND_IDLE" "" ""
assert_rc   "a type nothing uses is reported" 1
assert_says "  as the type, once" 'type `Ghost` has no production consumer'

run_against 'impl Ghost {
    pub fn new() -> Self { Ghost }
    pub fn len(&self) -> usize { 0 }
}' "$BACKEND_IDLE" 'Ghost stage-9 the stage that will wire it' ""
assert_rc   "a type-level exemption covers all its methods" 0

# --- exemptions are qualified too ------------------------------------
run_against "$ALPHA_NEW" "$BACKEND_WIRES_ALPHA" 'new stage-9 a bare name must not cover a method' ""
assert_rc   "a bare exemption does not cover a qualified method" 1
assert_says "  the qualified name is still reported" 'Alpha::new'

run_against "$ALPHA_NEW" "$BACKEND_WIRES_ALPHA" 'Alpha::new stage-9 qualified and ahead' ""
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
