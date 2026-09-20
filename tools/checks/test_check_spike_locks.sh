#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/test_check_spike_locks.sh
#
# Self-test for check_spike_locks.sh.
#
# The guard's whole value is the POSITIVE case: a lock that no longer
# resolves must be reported. A guard that cannot fail reports OK forever
# and reads as coverage -- which is the state the spike locks were
# actually in, undetected across two stages, until this guard was
# written.
#
# The sandbox builds real cargo workspaces rather than fixtures, because
# the question the guard asks is cargo's (`--locked` refuses to update a
# lock) and a mocked answer would prove nothing about it. Each case is
# self-contained: a tiny crate with no dependencies, so nothing is
# fetched from the network.
#
# Exit codes:
#   0  all assertions passed
#   1  one or more failed
#   2  cargo is unavailable, so the guard's own question cannot be posed

set -uo pipefail

SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
UNDER_TEST="$SCRIPT_DIR/check_spike_locks.sh"
[[ -f "$UNDER_TEST" ]] || { echo "test: $UNDER_TEST not found" >&2; exit 1; }

if ! command -v cargo >/dev/null 2>&1; then
    echo "test_check_spike_locks: cargo unavailable; the guard cannot be exercised." >&2
    exit 2
fi

failures=0
SANDBOX=""
cleanup() { [[ -n "$SANDBOX" && -d "$SANDBOX" ]] && rm -rf "$SANDBOX"; }
trap cleanup EXIT

pass() { echo "  ✓ $1"; }
fail() { echo "  ✗ $1" >&2; printf '%s\n' "${2:-}" | sed 's/^/      /' >&2
         failures=$((failures + 1)); }

# A throwaway repository holding one spike harness, laid out as the real
# tree lays them out: spikes/<name>/harness/{Cargo.toml,Cargo.lock,src}.
new_sandbox() {
    SANDBOX="$(mktemp -d)"
    mkdir -p "$SANDBOX/tools/checks" "$SANDBOX/spikes/spike-test/harness/src"
    cp "$UNDER_TEST" "$SANDBOX/tools/checks/"
    cat > "$SANDBOX/spikes/spike-test/harness/Cargo.toml" <<'EOF'
[package]
name = "spike-test-harness"
version = "0.0.0"
edition = "2021"

[dependencies]
EOF
    echo 'fn main() {}' > "$SANDBOX/spikes/spike-test/harness/src/main.rs"
}

# A sandbox that is a real git checkout with an `origin/main`, one spike
# harness pinning a revision, and a history the provenance phase can
# walk. `$PIN` is the commit the manifest names.
#
# `spike_only` decides whether the commit that FOLLOWS the pin touches
# only the spike's directory -- which is the property the phase asks
# about, so it must be the one thing the cases vary.
new_provenance_sandbox() {
    local spike_only="$1" merged="${2:-yes}"
    SANDBOX="$(mktemp -d)"
    mkdir -p "$SANDBOX/tools/checks"
    cp "$UNDER_TEST" "$SANDBOX/tools/checks/"
    git -C "$SANDBOX" init -q
    git -C "$SANDBOX" config user.email t@example.invalid
    git -C "$SANDBOX" config user.name t
    git -C "$SANDBOX" config commit.gpgsign false

    # The commit the pin names: production-side, outside the spike.
    mkdir -p "$SANDBOX/crates"
    echo 'the tree the run built against' > "$SANDBOX/crates/lib.rs"
    git -C "$SANDBOX" add -A >/dev/null
    git -C "$SANDBOX" commit -qm 'production'
    PIN="$( git -C "$SANDBOX" rev-parse HEAD )"

    mkdir -p "$SANDBOX/spikes/spike-test/harness/src"
    cat > "$SANDBOX/spikes/spike-test/harness/Cargo.toml" <<EOF
[package]
name = "spike-test-harness"
version = "0.0.0"
edition = "2021"

[dependencies]
# rev = "$PIN"
EOF
    echo 'fn main() {}' > "$SANDBOX/spikes/spike-test/harness/src/main.rs"
    ( cd "$SANDBOX/spikes/spike-test/harness" && cargo generate-lockfile -q --offline 2>/dev/null )
    if [[ "$spike_only" != "yes" ]]; then
        # THE ONE THING THAT VARIES: the recording commit also touches
        # production, so the crates it resolved by path are NOT its
        # parent's and the pin cannot be read off it.
        echo 'changed here too' >> "$SANDBOX/crates/lib.rs"
    fi
    git -C "$SANDBOX" add -A -f >/dev/null
    git -C "$SANDBOX" commit -qm 'the run'
    git -C "$SANDBOX" branch -M main
    if [[ "$merged" == "yes" ]]; then
        git -C "$SANDBOX" update-ref refs/remotes/origin/main HEAD
    else
        # The pin is a commit on a branch that never merged. Built as a
        # child of origin/main rather than by detaching, so the working
        # tree still holds the harness the earlier phases need.
        git -C "$SANDBOX" update-ref refs/remotes/origin/main HEAD
        PIN="$( git -C "$SANDBOX" commit-tree "HEAD^{tree}" -p HEAD -m 'never merged' )"
    fi
    sed -i "s/# rev = \"[0-9a-f]*\"/# rev = \"$PIN\"/" \
        "$SANDBOX/spikes/spike-test/harness/Cargo.toml"
}

run_guard() {
    RUN_OUT="$(cd "$SANDBOX" && bash tools/checks/check_spike_locks.sh 2>&1)"
    RUN_RC=$?
}

assert_rc() {
    if [[ "$RUN_RC" -eq "$2" ]]; then pass "$1"
    else fail "$1 — expected exit $2, got $RUN_RC" "$RUN_OUT"; fi
}
assert_contains() {
    if [[ "$RUN_OUT" == *"$2"* ]]; then pass "$1"
    else fail "$1 — output lacked '$2'" "$RUN_OUT"; fi
}

echo "check_spike_locks.sh — a committed spike lock that stopped resolving"

# A lock that matches its manifest resolves: the guard passes and says
# how many it looked at, so a zero-lock pass cannot be mistaken for a
# real one.
new_sandbox
( cd "$SANDBOX/spikes/spike-test/harness" && cargo generate-lockfile -q --offline 2>/dev/null )
run_guard
assert_rc "a lock that matches its manifest passes" 0
assert_contains "and says how many it checked" "1 committed spike lock"
rm -rf "$SANDBOX"; SANDBOX=""

# THE CASE THE GUARD EXISTS FOR: the manifest gained a dependency the
# committed lock does not name -- exactly the shape a root-manifest
# change produces in a harness that path-depends on production crates.
# A dependency that is itself in the lock would not do: the lock must be
# INCOMPLETE for the build it now describes.
new_sandbox
( cd "$SANDBOX/spikes/spike-test/harness" && cargo generate-lockfile -q --offline 2>/dev/null )
mkdir -p "$SANDBOX/spikes/spike-test/sibling/src"
cat > "$SANDBOX/spikes/spike-test/sibling/Cargo.toml" <<'EOF'
[package]
name = "spike-test-sibling"
version = "0.0.0"
edition = "2021"
EOF
echo '' > "$SANDBOX/spikes/spike-test/sibling/src/lib.rs"
cat >> "$SANDBOX/spikes/spike-test/harness/Cargo.toml" <<'EOF'
spike-test-sibling = { path = "../sibling" }
EOF
# MEASURED BEFORE THE RUN, not after. Taking it after a first run makes
# the assertion unfalsifiable: a guard that rewrites the lock does so
# during that run, and the second sees an already-fresh file. A review
# named the mutation that slipped through -- adding the printed remedy
# to the stale branch rewrites the lock AND still reports STALE, so
# every assertion here passed. That is the sibling guard's recorded
# incident exactly (review, PR #107).
before="$( md5sum < "$SANDBOX/spikes/spike-test/harness/Cargo.lock" )"
run_guard
assert_rc "a lock missing a package the manifest needs FAILS" 1
assert_contains "and names the lock that is stale" "spikes/spike-test/harness/Cargo.lock"
assert_contains "and says what to do about it" "cargo metadata --format-version 1"
# AND WARNS OFF THE DESTRUCTIVE FORM BY NAME. The guard used to print
# `cargo generate-lockfile` as the remedy, which rewrites the lock from
# scratch -- measured at eighty-odd packages moved on these three -- so
# following the guard's own advice destroyed the pinning it exists to
# protect. This assertion is what stops that text coming back.
assert_contains "and warns off the destructive form" "Do NOT run"
# THE GUARD MUST NOT REWRITE THE LOCK IT IS JUDGING. `--locked` refuses
# to update it, and that is the file's central claim.
after="$( md5sum < "$SANDBOX/spikes/spike-test/harness/Cargo.lock" )"
if [[ "$before" == "$after" ]]; then pass "and leaves the lock byte-identical"
else fail "and leaves the lock byte-identical — the guard rewrote it"; fi
rm -rf "$SANDBOX"; SANDBOX=""

# TWO LOCKS, ONE STALE: the loop accumulates rather than stopping at the
# first, and both are named. With a single lock per case nothing pins
# that -- `stale=1` or a `break` would pass every case above.
new_sandbox
( cd "$SANDBOX/spikes/spike-test/harness" && cargo generate-lockfile -q --offline 2>/dev/null )
mkdir -p "$SANDBOX/spikes/spike-two/harness/src"
cat > "$SANDBOX/spikes/spike-two/harness/Cargo.toml" <<'MANIFEST'
[package]
name = "spike-two-harness"
version = "0.0.0"
edition = "2021"

[workspace]

[dependencies]
MANIFEST
echo 'fn main() {}' > "$SANDBOX/spikes/spike-two/harness/src/main.rs"
( cd "$SANDBOX/spikes/spike-two/harness" && cargo generate-lockfile -q --offline 2>/dev/null )
mkdir -p "$SANDBOX/spikes/spike-two/sibling/src"
cat > "$SANDBOX/spikes/spike-two/sibling/Cargo.toml" <<'MANIFEST'
[package]
name = "spike-two-sibling"
version = "0.0.0"
edition = "2021"
MANIFEST
echo '' > "$SANDBOX/spikes/spike-two/sibling/src/lib.rs"
cat >> "$SANDBOX/spikes/spike-two/harness/Cargo.toml" <<'MANIFEST'
spike-two-sibling = { path = "../sibling" }
MANIFEST
# BOTH stale, not one: with a single stale lock among two, a `break`
# after the first and an accumulating loop are indistinguishable. Both
# named is what pins that the loop keeps going.
mkdir -p "$SANDBOX/spikes/spike-test/sibling/src"
cat > "$SANDBOX/spikes/spike-test/sibling/Cargo.toml" <<'MANIFEST'
[package]
name = "spike-test-sibling"
version = "0.0.0"
edition = "2021"
MANIFEST
echo '' > "$SANDBOX/spikes/spike-test/sibling/src/lib.rs"
cat >> "$SANDBOX/spikes/spike-test/harness/Cargo.toml" <<'MANIFEST'
spike-test-sibling = { path = "../sibling" }
MANIFEST
run_guard
assert_rc "two stale locks fail" 1
assert_contains "the first is named" "spikes/spike-test/harness/Cargo.lock is STALE"
assert_contains "and so is the second" "spikes/spike-two/harness/Cargo.lock is STALE"
rm -rf "$SANDBOX"; SANDBOX=""

# CARGO ABSENT IS EXIT 2, NOT A SILENT PASS. This is the file's most
# emphatic comment and nothing tested it: mutating its `exit 2` to
# `exit 0` broke no assertion.
new_sandbox
( cd "$SANDBOX/spikes/spike-test/harness" && cargo generate-lockfile -q --offline 2>/dev/null )
# Pointed at a cargo that is not there, rather than by emptying PATH --
# the script needs find, sed and the rest, so an empty PATH tests the
# harness and not the guard. That is the whole reason the guard honours
# $CARGO. (An earlier version of this comment also claimed it pins the
# invocation to the toolchain. It is the other way round under rustup:
# PATH holds the shim, which reads `rust-toolchain.toml`; $CARGO is
# cargo's own binary and bypasses it. No `rust-toolchain*` exists under
# `spikes/`, so the two agree here -- review, PR #107.)
RUN_OUT="$( cd "$SANDBOX" && CARGO=/nonexistent/cargo bash tools/checks/check_spike_locks.sh 2>&1 )"
RUN_RC=$?
assert_rc "cargo absent exits 2, not 0" 2
assert_contains "and names cargo as what is missing" "cargo is not available"
rm -rf "$SANDBOX"; SANDBOX=""

# A CARGO FAILURE THAT IS NOT ABOUT THE LOCK IS EXIT 2, NOT A FINDING.
# This is the branch the whole "environment problem" fix consists of,
# and nothing reached it: every other case either resolves or is stale,
# so `elif true` -- which is the original defect verbatim, every failure
# reported as STALE with a remedy pointing at the diff -- passed the
# entire suite (review, PR #107).
new_sandbox
( cd "$SANDBOX/spikes/spike-test/harness" && cargo generate-lockfile -q --offline 2>/dev/null )
printf '\n[[[ this is not toml\n' >> "$SANDBOX/spikes/spike-test/harness/Cargo.toml"
run_guard
assert_rc "a cargo failure that is not the lock exits 2" 2
assert_contains "and says it could not ask rather than blaming the lock" \
    "cargo failed for another reason"
rm -rf "$SANDBOX"; SANDBOX=""

# THE GIT ENUMERATION IS THE BRANCH CI TAKES, and no case reached it:
# every sandbox is a mktemp outside any work tree, so all of them took
# the `find` fallback. A wrong pathspec would have been a silent
# zero-lock pass in production with the suite fully green.
new_sandbox
( cd "$SANDBOX/spikes/spike-test/harness" && cargo generate-lockfile -q --offline 2>/dev/null )
git -C "$SANDBOX" init -q
git -C "$SANDBOX" add -f spikes/spike-test/harness/Cargo.lock >/dev/null 2>&1
run_guard
assert_rc "inside a checkout, the tracked lock is found and checked" 0
assert_contains "and counted" "1 committed spike lock"

# ...AND A CHECKOUT WITH HARNESSES BUT NO TRACKED LOCK IS EXIT 2, which
# is the failure mode the OK line exists to prevent: it looks exactly
# like success.
git -C "$SANDBOX" rm --cached -q spikes/spike-test/harness/Cargo.lock
run_guard
assert_rc "harnesses present but no tracked lock exits 2" 2
assert_contains "and says which way it could be wrong" "neither is a pass"
rm -rf "$SANDBOX"; SANDBOX=""

# INVOCATION PROBLEMS ARE 2 TOO.
RUN_OUT="$( bash "$UNDER_TEST" --nonsense 2>&1 )"; RUN_RC=$?
assert_rc "an unknown argument exits 2" 2
RUN_OUT="$( bash "$UNDER_TEST" --root 2>&1 )"; RUN_RC=$?
assert_rc "--root with no value exits 2" 2
RUN_OUT="$( bash "$UNDER_TEST" --root /nonexistent-dir-for-this-test 2>&1 )"; RUN_RC=$?
assert_rc "--root on a directory that cannot be entered exits 2" 2

# AND `--root` IS EXERCISED AT ALL, rather than only the copied-in form
# every other case uses.
new_sandbox
( cd "$SANDBOX/spikes/spike-test/harness" && cargo generate-lockfile -q --offline 2>/dev/null )
RUN_OUT="$( bash "$UNDER_TEST" --root "$SANDBOX" 2>&1 )"; RUN_RC=$?
assert_rc "--root <dir> checks that tree" 0
assert_contains "and reports its lock" "1 committed spike lock"
rm -rf "$SANDBOX"; SANDBOX=""
rm -rf "$SANDBOX"; SANDBOX=""

# A harness with no committed lock pins nothing and so cannot drift:
# reporting it would train a reader to ignore this guard's output.
new_sandbox
run_guard
assert_rc "a harness with no lock is not a failure" 0
assert_contains "and says so rather than passing silently" "no committed spike locks"
rm -rf "$SANDBOX"; SANDBOX=""

# A tree with no spikes at all: the same reasoning one level up.
new_sandbox
rm -rf "$SANDBOX/spikes"
run_guard
assert_rc "a tree with no spikes/ is not a failure" 0
assert_contains "and says why it had nothing to do" "no spikes/ directory"
rm -rf "$SANDBOX"; SANDBOX=""

# ---------------------------------------------------------------- build
#
# THE PHASE THAT EXISTS BECAUSE THE LOCK PHASE CANNOT SEE THIS. A lock
# that resolves says the dependency GRAPH is reproducible; it says
# nothing about whether the source type-checks against it. SPIKE-004 sat
# in exactly that state -- pinned at a revision missing three symbols
# `production.rs` imports -- and the guard reported OK across two
# stages (review, PR #109).
#
# The sandbox reproduces the MECHANISM rather than a lookalike: a lock
# that resolves cleanly, over a source rustc rejects. A missing
# dependency would not do -- that is the lock phase's finding, and it
# would pass this case for the wrong reason.
new_sandbox
( cd "$SANDBOX/spikes/spike-test/harness" && cargo generate-lockfile -q --offline 2>/dev/null )
echo 'fn main() { a_symbol_the_pinned_crate_does_not_define(); }' \
    > "$SANDBOX/spikes/spike-test/harness/src/main.rs"
run_guard
assert_rc "a harness whose lock resolves but whose source does not compile FAILS" 1
assert_contains "and the lock phase still passed, so the two are told apart" \
    "1 committed spike lock(s) resolve under --locked"
assert_contains "and names the harness that does not compile" \
    "spikes/spike-test/harness DOES NOT COMPILE"
assert_contains "and quotes rustc rather than a summary" "error[E"
assert_contains "and says re-running is what the pin was for" \
    "the only thing pinning it was for"
assert_contains "and points at the derivation rather than a bare rev" \
    "--first-parent"
rm -rf "$SANDBOX"; SANDBOX=""

# THE PASSING CASE SAYS BOTH PHASES RAN. Without this, deleting the
# whole build loop leaves every other assertion green -- the OK line is
# the only place the second phase is visible on success.
new_sandbox
( cd "$SANDBOX/spikes/spike-test/harness" && cargo generate-lockfile -q --offline 2>/dev/null )
run_guard
assert_rc "a harness that resolves and compiles passes" 0
assert_contains "and says the harness compiled" "compiles at its pinned revisions"
assert_contains "and the OK line claims both phases" "resolve and compile"
rm -rf "$SANDBOX"; SANDBOX=""

# `--no-build` SKIPS THE PHASE AND SAYS SO. A flag that silently
# narrowed what the guard asks would be the failure mode this whole
# file is written against: it must not be possible to read a
# resolve-only run as a full pass.
new_sandbox
( cd "$SANDBOX/spikes/spike-test/harness" && cargo generate-lockfile -q --offline 2>/dev/null )
echo 'fn main() { a_symbol_the_pinned_crate_does_not_define(); }' \
    > "$SANDBOX/spikes/spike-test/harness/src/main.rs"
RUN_OUT="$(cd "$SANDBOX" && bash tools/checks/check_spike_locks.sh --no-build 2>&1)"
RUN_RC=$?
assert_rc "--no-build passes the harness that does not compile" 0
assert_contains "and says the harnesses were not compiled" "were not compiled"
if [[ "$RUN_OUT" != *"resolve and compile"* ]]; then
    pass "and does not claim the build phase ran"
else
    fail "and does not claim the build phase ran — the OK line overclaims" "$RUN_OUT"
fi
rm -rf "$SANDBOX"; SANDBOX=""

# A STALE LOCK DOES NOT REACH THE BUILD PHASE. `cargo check --locked`
# would fail there for the reason already reported, under a worse
# description -- two findings for one defect, the second misleading.
new_sandbox
( cd "$SANDBOX/spikes/spike-test/harness" && cargo generate-lockfile -q --offline 2>/dev/null )
mkdir -p "$SANDBOX/spikes/spike-test/sibling/src"
cat > "$SANDBOX/spikes/spike-test/sibling/Cargo.toml" <<'MANIFEST'
[package]
name = "spike-test-sibling"
version = "0.0.0"
edition = "2021"
MANIFEST
echo '' > "$SANDBOX/spikes/spike-test/sibling/src/lib.rs"
cat >> "$SANDBOX/spikes/spike-test/harness/Cargo.toml" <<'MANIFEST'
spike-test-sibling = { path = "../sibling" }
MANIFEST
run_guard
assert_rc "a stale lock still fails" 1
if [[ "$RUN_OUT" != *"DOES NOT COMPILE"* ]]; then
    pass "and the build phase is not reached"
else
    fail "and the build phase is not reached — it ran on a stale lock" "$RUN_OUT"
fi
rm -rf "$SANDBOX"; SANDBOX=""


# ------------------------------------------------------------ provenance
#
# THE PIN HAS BEEN WRONG TWICE, and both times it passed everything that
# existed. `cf04e7b7` came from the verdict's date and did not compile;
# `9d66a24c` came from the last run's DATE, compiled, and would have
# failed recorded rows. These two properties are what a date-derived pin
# fails, and they are cheap -- they are NOT the proof, which is a
# reproduction run (PR #110).

new_provenance_sandbox yes yes
run_guard
assert_rc "a pin that is an ancestor of origin/main and parents a spike-only commit passes" 0
assert_contains "and says what it traced it to" "on origin/main, parent of"
rm -rf "$SANDBOX"; SANDBOX=""

# NOT AN ANCESTOR: a feature-branch tip that never merged, which is what
# the first version of these pins recorded.
new_provenance_sandbox yes no
run_guard
assert_rc "a pin that never merged FAILS" 1
assert_contains "and says it is not on origin/main" "NOT an ancestor of origin/main"
assert_contains "and says why that matters" "never merged"
rm -rf "$SANDBOX"; SANDBOX=""

# THE DATE-DERIVED SHAPE: the pin is on main, but the commit after it
# touches production as well as the spike, so it cannot be a recording
# commit and the pin is not the tree a run built against.
new_provenance_sandbox no yes
run_guard
assert_rc "a pin that parents no spike-only commit FAILS" 1
assert_contains "and names what it looked for" "parent of no commit"
assert_contains "and names the date derivation as the way in" "DATE"
rm -rf "$SANDBOX"; SANDBOX=""

# `--no-provenance` SKIPS IT, for a clone without the history, and the
# skip is visible.
new_provenance_sandbox yes no
RUN_OUT="$(cd "$SANDBOX" && bash tools/checks/check_spike_locks.sh --no-provenance 2>&1)"
RUN_RC=$?
assert_rc "--no-provenance passes the pin that never merged" 0
if [[ "$RUN_OUT" != *"NOT an ancestor"* ]]; then
    pass "and does not report it"
else
    fail "and does not report it — the phase ran anyway" "$RUN_OUT"
fi
rm -rf "$SANDBOX"; SANDBOX=""

# A CARGO FAILURE IN THE BUILD PHASE THAT IS NOT A COMPILE ERROR IS
# EXIT 2, NOT A FINDING. Phase one has this case and phase two did not,
# so widening its grep to `-e 'error'` would silently reclassify every
# environment failure as a finding with the suite green (review,
# PR #110).
new_sandbox
( cd "$SANDBOX/spikes/spike-test/harness" && cargo generate-lockfile -q --offline 2>/dev/null )
# CARGO_TARGET_DIR, not RUSTC: a missing rustc fails `cargo metadata`
# too, so the run never reaches the build phase and the case would pass
# on phase one's exit 2 instead -- measured. An unwritable target
# directory leaves metadata working and fails check before rustc.
RUN_OUT="$(cd "$SANDBOX" && CARGO_TARGET_DIR=/proc/nope bash tools/checks/check_spike_locks.sh 2>&1)"
RUN_RC=$?
assert_rc "a build failure that never reached rustc exits 2" 2
assert_contains "and says it could not ask rather than blaming the pin" \
    "cargo failed before rustc"
rm -rf "$SANDBOX"; SANDBOX=""

if (( failures > 0 )); then
    echo "test_check_spike_locks: $failures assertion(s) failed." >&2
    exit 1
fi
echo "test_check_spike_locks: OK — all assertions passed."
