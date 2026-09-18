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
             "$SANDBOX/crates/transport/libp2p/src" \
             "$SANDBOX/spikes/spike-000/harness/src" \
             "$SANDBOX/third_party/vendored-crate/src"
    cp "$UNDER_TEST" "$SANDBOX/tools/checks/"
    printf '%s\n' "$1" > "$SANDBOX/crates/transport/runtime/src/lib.rs"
    printf '%s\n' "$2" > "$SANDBOX/crates/transport/libp2p/src/lib.rs"
    printf '%s\n' "$3" > "$SANDBOX/tools/checks/domain_fn_exempt.txt"
    printf 'status = "%s"\n' "${4:-stage-6-direct-v2}" > "$SANDBOX/Cargo.toml"
    printf '%s\n' "${5:-}" > "$SANDBOX/spikes/spike-000/harness/src/main.rs"
    printf '%s\n' "${6:-}" > "$SANDBOX/third_party/vendored-crate/src/lib.rs"
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

# A call inside the DEFINING file DOES satisfy an explicit exemption,
# and only through this path. The ordinary caller scan still ignores the
# defining file; what is different here is that a human wrote the call
# down and this check verifies the exact text exists. Without it a
# genuine caller is inexpressible: `TrustSources::classify` is called by
# `self.trust.classify` inside its own file.
run_against 'impl Refusal {
    pub fn to_wire(&self) -> u8 { 0 }
}
fn local(r: &Refusal) -> u8 { refusal.to_wire() }' 'fn go(_r: &Refusal) {}' \
    'Refusal::to_wire call refusal.to_wire(' ""
assert_rc   "an explicit call exemption may name a call in the defining file" 0

# But the ordinary scan is unchanged: without the exemption, a same-file
# caller is still not a caller.
run_against 'impl Refusal {
    pub fn to_wire(&self) -> u8 { 0 }
}
fn local(r: &Refusal) -> u8 { refusal.to_wire() }' 'fn go(_r: &Refusal) {}' "" ""
assert_rc   "  and same-file use alone still does not count" 1

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

# A brace in a test module's comment, string or char literal must not
# end the module early: everything below would read as production, and
# a method only a test reads would be reported as read (PR #91, where
# the counter hit zero inside a comment and eight hundred test lines
# leaked).
run_against 'impl Alpha {
    pub fn probe(&self) -> u8 { 0 }
}' 'fn go(_a: &Alpha) {}
#[cfg(test)]
mod t {
    // a comment with a stray }
    const S: &str = "a string with }";
    const C: char = '"'"'}'"'"';
    fn probe_it(a: &Alpha) { let _ = a.probe(); }
}' "" ""
assert_rc   "a brace in a test comment or literal does not leak the module into production" 1

# A literal spanning lines: per-line blanking paired its quotes wrongly
# and exposed the JSON's braces, which closed the module early
# (`profile-config`'s and `discovery-api`'s documents; PR #91, round 3).
run_against 'impl Alpha {
    pub fn probe(&self) -> u8 { 0 }
}' 'fn go(_a: &Alpha) {}
#[cfg(test)]
mod t {
    const J: &str = r#"{"a":
"b"}}}"#;
    fn probe_it(a: &Alpha) { let _ = a.probe(); }
}' "" ""
assert_rc   "a literal spanning lines does not leak the module into production" 1

# An attributed item INSIDE an impl closes at its own indentation, not
# at column zero: a `#[cfg(test)]` method must not swallow the
# production methods after it.
run_against 'impl Alpha {
    pub fn probe(&self) -> u8 { 0 }
}' 'struct Beta;
impl Beta {
    #[cfg(test)]
    fn only_in_tests(&self, a: &Alpha) -> u8 {
        a.probe()
    }

    fn go(&self, a: &Alpha) -> u8 {
        a.probe()
    }
}' "" ""
assert_rc   "an attributed method inside an impl ends at its own closing brace" 0

# --- every item shape rustfmt closes, from the audit on PR #91 --------
#
# Each case fails on the rule before its own: the comment on
# `strip_test_items` names the clause each one pins.
ALPHA_PROBE='impl Alpha {
    pub fn probe(&self) -> u8 { 0 }
}'

# A `};` closer: a use tree, an initializer. Ended at `}` alone, the
# skip ran on to the next item's closing brace.
run_against "$ALPHA_PROBE" '#[cfg(test)]
use std::collections::{
    BTreeMap, HashMap,
};

fn go(a: &Alpha) { let _ = a.probe(); }' "" ""
assert_rc   "production after an attributed multi-line use tree is still read" 0

run_against "$ALPHA_PROBE" '#[cfg(test)]
static FIXTURE: LazyLock<Config> = LazyLock::new(|| Config {
    a: 1,
});

fn go(a: &Alpha) { let _ = a.probe(); }' "" ""
assert_rc   "an attributed initializer closing with }); ends there" 0

# The silent shape: a `};`-closed item last in an impl, a test module
# after it -- the skip ate the impl's brace and half the module, and a
# unit test read as production vouched for the method.
run_against "$ALPHA_PROBE" 'struct Beta;
impl Beta {
    #[cfg(test)]
    const X: Foo = Foo {
        a: 1,
    };
}
#[cfg(test)]
mod tests {
    #[test]
    fn first() {
    }
    #[test]
    fn second(a: &Alpha) { let _ = a.probe(); }
}' "" ""
assert_rc   "a }; item last in an impl does not leak the test module after it" 1

# A paren-delimited item does not end at an inner `;`.
run_against "$ALPHA_PROBE" 'fn go(_a: &Alpha) {}
#[cfg(test)]
thread_local!(
    static FIRST: u8 = 1;
    static SECOND: fn(&Alpha) -> u8 = |a| a.probe();
);' "" ""
assert_rc   "a paren-delimited test item does not end at an inner semicolon" 1

run_against "$ALPHA_PROBE" '#[cfg(test)]
const OPEN: char = '"'"'('"'"';

fn go(a: &Alpha) { let _ = a.probe(); }' "" ""
assert_rc   "a char literal holding a paren does not hold the item open" 0

run_against "$ALPHA_PROBE" '#[cfg(test)] use std::fmt;

fn go(a: &Alpha) { let _ = a.probe(); }' "" ""
assert_rc   "an attribute and its item on one line end on that line" 0

run_against "$ALPHA_PROBE" 'struct Beta {
    #[cfg(test)]
    seen: u8,
    inner: u8,
}
fn go(a: &Alpha) { let _ = a.probe(); }' "" ""
assert_rc   "an attributed struct field swallows only itself" 0

run_against "$ALPHA_PROBE" 'fn go(_a: &Alpha) {}
#[cfg(test)]
const PROBED: u8 = Alpha {
    x: 1,
}
.probe();' "" ""
assert_rc   "a chain continuing an attributed initializer is part of the item" 1

run_against "$ALPHA_PROBE" 'fn go(_a: &Alpha) {}
#[cfg(all(test, feature = "x"))]
mod t { fn probe_it(a: &Alpha) { let _ = a.probe(); } }' "" ""
assert_rc   "cfg(all(test, ..)) is a test item too" 1

run_against "$ALPHA_PROBE" 'fn go(_a: &Alpha) {}
#[cfg(all(feature = "x", test))]
mod t { fn probe_it(a: &Alpha) { let _ = a.probe(); } }' "" ""
assert_rc   "and so is cfg(all(.., test)) with test last" 1

# `any(test, ..)` is compiled into a production build with the other
# condition: its item is production, and a caller there is a caller.
run_against "$ALPHA_PROBE" '#[cfg(any(test, feature = "x"))]
fn go(a: &Alpha) { let _ = a.probe(); }' "" ""
assert_rc   "cfg(any(test, ..)) is production" 0

# Round-5 findings on PR #91: the closers the comment named without a
# case, and the shapes it called impossible.
run_against "$ALPHA_PROBE" '#[cfg(test)]
static X: Foo = Foo::new(
    Bar {
        a: 1,
    },
);

fn go(a: &Alpha) { let _ = a.probe(); }' "" ""
assert_rc   "a paren-closed initializer with a brace-opening argument ends at its );" 0

run_against "$ALPHA_PROBE" 'fn go(_a: &Alpha) {}
#[cfg(test)]
fn helper(
    n: [u8; { 2 }],
) -> u8 { let a = Alpha; a.probe() }' "" ""
assert_rc   "a balanced-brace parameter inside an open paren does not end the header" 1

run_against "$ALPHA_PROBE" 'fn go(_a: &Alpha) {}
#[cfg(test)]
const PROBED: u8 = if cfg!(feature = "x") {
    0
} else {
    Alpha.probe()
};' "" ""
assert_rc   "a } else { at the attribute indentation continues the item" 1

# The item CLOSES at `}` before the dot-line, so the dot-line is read by
# the tail rule and nothing else: an opener that leaves the item open
# would exercise the header rule instead and pin nothing here (PR #91,
# round 6).
run_against "$ALPHA_PROBE" 'fn go(_a: &Alpha) {}
#[cfg(test)]
const N: usize = Foo {
    a: 1,
}
.map(|a| {
    a.probe()
})
.count();' "" ""
assert_rc   "a chain element opening a brace on a dot-line is part of the item" 1

run_against "$ALPHA_PROBE" 'fn go(_a: &Alpha) {}
#[cfg(test)]
const N: usize = Foo {
    a: 1,
}
.map(
    |a| a.probe(),
)
.count();' "" ""
assert_rc   "a chain element opening a paren on a dot-line is part of the item" 1

# The swallowed line must be the ONLY mention of the domain type, or
# the case passes whether or not it was swallowed (PR #91, round 6).
run_against "$ALPHA_PROBE" 'struct Beta {
    #[cfg(test)] seen: u8,
    inner: Alpha,
}
fn go(b: &Beta) { let _ = b.inner.probe(); }' "" ""
assert_rc   "a one-line attribute on a field swallows only that field" 0

run_against "$ALPHA_PROBE" 'fn go(_a: &Alpha) {}
#[cfg(all(not(feature = "x"), test))]
mod t { fn probe_it(a: &Alpha) { let _ = a.probe(); } }' "" ""
assert_rc   "cfg(all(not(..), test)) is a test item too" 1

run_against "$ALPHA_PROBE" '#[cfg(test)]
static ARR: [Foo; 1] = [
    Foo {
        a: 1,
    },
];

fn go(a: &Alpha) { let _ = a.probe(); }' "" ""
assert_rc   "an attributed array initializer ends at its ];" 0

run_against "$ALPHA_PROBE" '#[cfg(test)]
type Wide = Map<
    A,
    B,
>;

fn go(a: &Alpha) { let _ = a.probe(); }' "" ""
assert_rc   "an attributed type alias with a broken generic list ends at its >;" 0

run_against "$ALPHA_PROBE" '#[cfg(test)]
fn helper(
    a: u8,
) -> u8 { 0 }

fn go(a: &Alpha) { let _ = a.probe(); }' "" ""
assert_rc   "a header that closes its paren and its body on one line ends there" 0

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

# --- a spike is not a caller ------------------------------------------
#
# The SPIKE-003 harness depends on the production crates by path, which
# CLAUDE.md §4 permits. It then called a domain function and this guard
# reported it as called — a spike vouching for a rule nothing in
# production applies is the same false green a unit test would give.
run_against "$UNCALLED" "$BACKEND_IDLE" "" "" \
    'fn evidence() { let _ = authorize_outbound(1); }'
assert_rc   "a spike harness is not a production caller" 1
assert_says "  and the function is still reported" 'authorize_outbound'

run_against "$UNCALLED" 'fn go() { let _ = authorize_outbound(1); }' "" "" \
    'fn evidence() { let _ = authorize_outbound(1); }'
assert_rc   "CONTROL: a real production caller in the same shape passes" 0

# --- a vendored dependency is not a caller either ----------------------
#
# `third_party/` holds crates this repository compiles but did not write
# (ADR-0051). A vendored file that happens to use one of our names must
# not vouch for it.
#
# THE EXCLUSION IS DEFENSIVE, NOT LOAD-BEARING, and this comment claimed the
# opposite for three revisions. `libp2p-autonat` does declare its own
# `DialRequest`, and `DialRequest` is a domain TYPE here -- but there is no
# top-level `impl DialRequest` anywhere in `crates/api/` or
# `crates/transport/runtime/`, so the guard never indexes it as an owner,
# a method or a free function. It is an identifier this script never looks
# up, and it can vouch for nothing in either direction. The only owner type
# that collides at all is `TransportError`, and only under
# `third_party/libp2p-autonat/tests/`, which the pre-existing `tests/`
# clause already drops.
#
# The guard's own copy of this claim was corrected twice and this one was
# left behind both times -- CLAUDE.md section 7's named shape, where the
# reasoning is right in the file you are editing and its counterpart lives
# in the file you are not. Read `check_domain_fns_are_called.sh`'s
# paragraph with this one; they are a pair. Review findings on PR #85.
run_against "$UNCALLED" "$BACKEND_IDLE" "" "" "" \
    'fn upstream() { let _ = authorize_outbound(1); }'
assert_rc   "a vendored dependency is not a production caller" 1
assert_says "  and the function is still reported" 'authorize_outbound'

run_against "$UNCALLED" 'fn go() { let _ = authorize_outbound(1); }' "" "" "" \
    'fn upstream() { let _ = authorize_outbound(1); }'
assert_rc   "CONTROL: a real production caller in the same shape passes" 0

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
