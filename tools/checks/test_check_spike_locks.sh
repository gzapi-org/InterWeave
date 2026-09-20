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
    # THE SHAPE THE REAL MANIFESTS CARRY: a dependency line naming this
    # repository by git and carrying its own rev. Two earlier versions of
    # this fixture were weaker and each hid a defect -- `# rev = "..."`
    # in a comment, which only matched because the pattern did not skip
    # comments, and then a bare `rev = "..."` on its own line, which
    # stopped matching once the rev had to come from the line that names
    # the repository (review, PR #110).
    #
    # UNDER `[package.metadata]`, because a real `[dependencies]` git
    # entry would need the network to resolve and the lock phase runs
    # first; these sandboxes are offline. Cargo ignores the table and the
    # guard greps the text, so the LINE is what the guard reads even
    # though the SECTION is not one cargo would resolve.
    cat > "$SANDBOX/spikes/spike-test/harness/Cargo.toml" <<EOF
[package]
name = "spike-test-harness"
version = "0.0.0"
edition = "2021"

[package.metadata.spike]
interweave-transport-libp2p = { git = "https://github.com/gzapi-org/InterWeave.git", rev = "$PIN" }

[dependencies]
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
    sed -i "s/rev = \"[0-9a-f]*\" }/rev = \"$PIN\" }/" \
        "$SANDBOX/spikes/spike-test/harness/Cargo.toml"
}

# THE DEFAULT RUNNER OPTS OUT OF PROVENANCE, because these sandboxes are
# mktemp directories outside any work tree and the phase refuses that
# state by design -- an exported tree cannot say where a pin came from,
# and a quiet skip followed by "the pins are accounted for" is the silent
# pass the whole file is written against. The cases that exercise the
# phase build a real checkout and use `run_provenance_guard`.
run_guard() {
    RUN_OUT="$(cd "$SANDBOX" && bash tools/checks/check_spike_locks.sh --no-provenance 2>&1)"
    RUN_RC=$?
}

run_provenance_guard() {
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
RUN_OUT="$( cd "$SANDBOX" && CARGO=/nonexistent/cargo bash tools/checks/check_spike_locks.sh --no-provenance 2>&1 )"
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
RUN_OUT="$( bash "$UNDER_TEST" --root "$SANDBOX" --no-provenance 2>&1 )"; RUN_RC=$?
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
    "the spike's OWN history"
# AND IT MUST NOT TEACH THE ONE THE RULE RETIRED. The remedy printed
# `git rev-list --before=<date>` -- the date derivation this same change
# documents as wrong, and the one that put three pins wrong. A guard
# whose advice recreates the defect is worse than one that says nothing
# (review, PR #110).
if [[ "$RUN_OUT" != *"rev-list -1 --first-parent --before="* ]]; then
    pass "and does not tell the reader to derive the pin from a date"
else
    fail "and does not tell the reader to derive the pin from a date" "$RUN_OUT"
fi
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
RUN_OUT="$(cd "$SANDBOX" && bash tools/checks/check_spike_locks.sh --no-build --no-provenance 2>&1)"
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
run_provenance_guard
assert_rc "a pin that is an ancestor of origin/main and parents a spike-only commit passes" 0
assert_contains "and says what it traced it to" "on origin/main, parent of"
rm -rf "$SANDBOX"; SANDBOX=""

# NOT AN ANCESTOR: a feature-branch tip that never merged, which is what
# the first version of these pins recorded.
new_provenance_sandbox yes no
run_provenance_guard
assert_rc "a pin that never merged FAILS" 1
assert_contains "and says it is not on origin/main" "NOT an ancestor of origin/main"
assert_contains "and says why that matters" "never merged"
rm -rf "$SANDBOX"; SANDBOX=""

# THE DATE-DERIVED SHAPE: the pin is on main, but the commit after it
# touches production as well as the spike, so it cannot be a recording
# commit and the pin is not the tree a run built against.
new_provenance_sandbox no yes
run_provenance_guard
assert_rc "a pin that parents no spike-only commit FAILS" 1
assert_contains "and names what it looked for" "no commit in that"
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
# EXIT 2, NOT A FINDING. Phase one has this case and the BUILD phase did not,
# so widening its grep to `-e 'error'` would silently reclassify every
# environment failure as a finding with the suite green (review,
# PR #110).
new_sandbox
( cd "$SANDBOX/spikes/spike-test/harness" && cargo generate-lockfile -q --offline 2>/dev/null )
# CARGO_TARGET_DIR, not RUSTC: a missing rustc fails `cargo metadata`
# too, so the run never reaches the build phase and the case would pass
# on phase one's exit 2 instead -- measured. An unwritable target
# directory leaves metadata working and fails check before rustc.
RUN_OUT="$(cd "$SANDBOX" && CARGO_TARGET_DIR=/proc/nope bash tools/checks/check_spike_locks.sh --no-provenance 2>&1)"
RUN_RC=$?
assert_rc "a build failure that never reached rustc exits 2" 2
assert_contains "and says it could not ask rather than blaming the pin" \
    "cargo failed before rustc"
rm -rf "$SANDBOX"; SANDBOX=""


# A MERGE COMMIT IS NOT A RECORDING, and this case exists because the
# first version of the phase accepted one. `git show --name-only` prints
# NOTHING for a merge, so the "touches only the spike" test saw an empty
# file list, computed zero files outside, and passed -- which made the
# phase vacuous for nearly every pin on `main`. Found by running the
# phase against this repository and reading which commit it named
# (spike-002's pin resolved to the merge 1a345aa).
new_provenance_sandbox yes yes
# AN EVIL MERGE, because an ordinary one proves nothing here. The walk
# is path-filtered, and git's history simplification already drops a
# merge that is TREESAME to a parent -- measured at 34/34, 43/43 and
# 149/149 on this repository, with and without `--no-merges`. What the
# flag actually excludes is a merge whose tree differs from both parents
# under the spike's directory: that one is NOT simplified away, its file
# list is non-empty and confined to the spike, and without the flag it
# would be accepted as a recording commit. The first version of this
# case built a TREESAME merge and passed on the "no commit points at the
# pin" path instead, so it would have survived deleting the flag
# (review, PR #110).
# The merge's tree differs from BOTH parents inside the spike, which is
# what makes it evil and keeps it in a path-filtered walk.
echo '// only in the merge' >> "$SANDBOX/spikes/spike-test/harness/src/main.rs"
git -C "$SANDBOX" add -A -f >/dev/null
evil_tree="$( git -C "$SANDBOX" write-tree )"
# FROM HEAD, not from the index: `git checkout -- <paths>` copies out of
# the INDEX, which `git add -A` had just updated, so the line stayed in
# both index and worktree and this read as a rollback while doing
# nothing (review, PR #110).
git -C "$SANDBOX" checkout -q HEAD -- .
side_tree="$( git -C "$SANDBOX" rev-parse 'HEAD^{tree}' )"
# `--verify`, because a plain `git rev-parse <root>^` PRINTS its
# argument to stdout and exits non-zero, so `base` came out as the
# literal "<sha>^", every later git call failed, origin/main stayed
# where it was, and the case passed against a tree it never built
# (measured).
base="$( git -C "$SANDBOX" rev-parse --verify --quiet "$PIN^" 2>/dev/null || true )"
if [[ -n "$base" ]]; then
    side="$( git -C "$SANDBOX" commit-tree "$side_tree" -p "$base" -m 'the run' )"
else
    side="$( git -C "$SANDBOX" commit-tree "$side_tree" -m 'the run' )"
fi
merge="$( git -C "$SANDBOX" commit-tree "$evil_tree" -p "$PIN" -p "$side" -m 'merge' )"
git -C "$SANDBOX" update-ref refs/remotes/origin/main "$merge"
run_provenance_guard
assert_rc "a pin whose only child is a MERGE fails" 1
assert_contains "and does not accept an evil merge as a recording commit" "no commit in that"
rm -rf "$SANDBOX"; SANDBOX=""


# SHAPE TWO: a recording commit that ALSO changes a production crate
# measured the code it landed with, so the pin is that commit ITSELF
# rather than its parent (architect-cto, c2f8c0b). Without this case the
# phase could accept only shape one and every shape-two pin would be
# reported as unaccounted.
new_provenance_sandbox yes yes
# Make the spike commit touch a crate as well, and point the pin at it.
mkdir -p "$SANDBOX/crates"
echo 'measured with the run' >> "$SANDBOX/crates/lib.rs"
echo '// a row' >> "$SANDBOX/spikes/spike-test/harness/src/main.rs"
git -C "$SANDBOX" add -A -f >/dev/null
git -C "$SANDBOX" commit -qm 'the run, with the crate it measured'
git -C "$SANDBOX" update-ref refs/remotes/origin/main HEAD
PIN="$( git -C "$SANDBOX" rev-parse HEAD )"
sed -i "s/rev = \"[0-9a-f]*\" }/rev = \"$PIN\" }/" \
    "$SANDBOX/spikes/spike-test/harness/Cargo.toml"
run_provenance_guard
assert_rc "a pin that IS a recording commit touching a crate passes" 0
assert_contains "and says it pointed at the commit itself" "itself,"
rm -rf "$SANDBOX"; SANDBOX=""

# ...AND SHAPE TWO IS NOT A LOOPHOLE. A commit touching the spike and
# something that is NOT a production crate does not qualify, or "the pin
# is any commit that touched this spike" would pass for anything.
new_provenance_sandbox yes yes
mkdir -p "$SANDBOX/architecture"
echo 'prose, not a crate' >> "$SANDBOX/architecture/note.md"
echo '// a row' >> "$SANDBOX/spikes/spike-test/harness/src/main.rs"
git -C "$SANDBOX" add -A -f >/dev/null
git -C "$SANDBOX" commit -qm 'the spike and some prose'
git -C "$SANDBOX" update-ref refs/remotes/origin/main HEAD
PIN="$( git -C "$SANDBOX" rev-parse HEAD )"
sed -i "s/rev = \"[0-9a-f]*\" }/rev = \"$PIN\" }/" \
    "$SANDBOX/spikes/spike-test/harness/Cargo.toml"
run_provenance_guard
assert_rc "a pin that is a commit touching the spike and NO crate fails" 1
assert_contains "and says where the search starts" "OWN history"
rm -rf "$SANDBOX"; SANDBOX=""


# THE TWO STATES THAT CANNOT ANSWER, and neither is a pass. Both were
# silent failures in the first version of the phase (review, PR #110).

# Outside a checkout the phase used to skip itself and let the run reach
# the OK line, which says "the pins are accounted for" -- a sentence that
# had not been established. `--root` reaches exactly this state.
new_sandbox
( cd "$SANDBOX/spikes/spike-test/harness" && cargo generate-lockfile -q --offline 2>/dev/null )
run_provenance_guard
assert_rc "provenance outside a checkout exits 2, not 0" 2
assert_contains "and says which knob means it on purpose" "--no-provenance"
if [[ "$RUN_OUT" != *"accounted for"* ]]; then
    pass "and never claims the pins are accounted for"
else
    fail "and never claims the pins are accounted for" "$RUN_OUT"
fi
rm -rf "$SANDBOX"; SANDBOX=""

# ...AND --no-provenance IS THE WAY THROUGH, so the strictness above does
# not make an exported tree uncheckable.
new_sandbox
( cd "$SANDBOX/spikes/spike-test/harness" && cargo generate-lockfile -q --offline 2>/dev/null )
RUN_OUT="$(cd "$SANDBOX" && bash tools/checks/check_spike_locks.sh --no-provenance 2>&1)"
RUN_RC=$?
assert_rc "--no-provenance passes outside a checkout" 0
rm -rf "$SANDBOX"; SANDBOX=""

# A CHECKOUT WITH NO origin/main IS NOT A TREE FULL OF BAD PINS. Without
# the ref, `git merge-base --is-ancestor` exits 128 for a bad revision;
# `2>/dev/null` made that the same answer as 1, so every valid pin was
# reported as an unmerged feature tip and the run exited 1 with a finding
# it had invented.
new_provenance_sandbox yes yes
git -C "$SANDBOX" update-ref -d refs/remotes/origin/main
run_provenance_guard
assert_rc "a checkout with no origin/main exits 2, not 1" 2
assert_contains "and names the ref that is missing" "no origin/main"
if [[ "$RUN_OUT" != *"NOT an ancestor"* ]]; then
    pass "and does not report the pin as unmerged"
else
    fail "and does not report the pin as unmerged — 128 read as 'not an ancestor'" "$RUN_OUT"
fi
rm -rf "$SANDBOX"; SANDBOX=""


# ONE UNPINNED DEPENDENCY MUST NOT HIDE BEHIND A PINNED SIBLING. The gap
# check used to ask whether the MANIFEST held any rev at all, so a
# harness with two git dependencies on this repository -- one pinned,
# one floating -- passed, and the OK line said the pins were accounted
# for while a production crate floated (review, PR #110).
new_provenance_sandbox yes yes
# UNDER `[package.metadata]`, because a real `[dependencies]` git entry
# would need the network to resolve and the lock phase runs first --
# these sandboxes are offline. Cargo ignores the table; the guard greps
# the text, so the two lines are the shape it reads. Same caveat as the
# pin fixture: close to what the caller holds, not identical to it.
cat >> "$SANDBOX/spikes/spike-test/harness/Cargo.toml" <<EOF

[package.metadata.deps-under-test]
pinned-one = { git = "https://github.com/gzapi-org/InterWeave.git", rev = "$PIN" }
floating-one = { git = "https://github.com/gzapi-org/InterWeave.git" }
EOF
run_provenance_guard
assert_rc "a git dependency on this repository with no rev FAILS" 1
assert_contains "and quotes the line that has none" "floating-one"
if [[ "$RUN_OUT" != *"pinned-one"* ]]; then
    pass "and does not blame the sibling that is pinned"
else
    fail "and does not blame the sibling that is pinned" "$RUN_OUT"
fi
rm -rf "$SANDBOX"; SANDBOX=""

# A SHALLOW CLONE IS UNANSWERABLE, NOT A TREE OF BAD PINS. With the
# connecting history absent, `--is-ancestor` is a reachability query
# that returns 1 -- the same answer as a genuine non-ancestor -- so
# every valid pin would be reported as an unmerged feature tip.
new_provenance_sandbox yes yes
touch "$SANDBOX/.git/shallow"
run_provenance_guard
assert_rc "a shallow clone exits 2, not 1" 2
assert_contains "and says what cannot be answered" "shallow clone"
if [[ "$RUN_OUT" != *"NOT an ancestor"* ]]; then
    pass "and does not report the pin as unmerged"
else
    fail "and does not report the pin as unmerged" "$RUN_OUT"
fi
rm -rf "$SANDBOX"; SANDBOX=""


# THE SPELLINGS CARGO ACCEPTS AND THE PATTERN USED NOT TO. A short rev,
# no spaces around `=`, upper-case hex, TOML literal quotes -- each is a
# legal pin that matched nothing, so the manifest went untraced while the
# OK line said the pins were accounted for (review, PR #110).
new_provenance_sandbox yes yes
short="${PIN:0:10}"
cat >> "$SANDBOX/spikes/spike-test/harness/Cargo.toml" <<EOF

[package.metadata.spellings]
unspaced = { git="https://github.com/gzapi-org/InterWeave.git", rev="$short" }
literal = { git = 'https://github.com/gzapi-org/InterWeave.git', rev = '$PIN' }
EOF
run_provenance_guard
assert_rc "an abbreviated, unspaced or literal-quoted pin is still traced" 0
if [[ "$RUN_OUT" != *"no revision this check can read"* ]]; then
    pass "and none of them is reported as unreadable"
else
    fail "and none of them is reported as unreadable" "$RUN_OUT"
fi
rm -rf "$SANDBOX"; SANDBOX=""

# A REV BELONGING TO SOMEONE ELSE IS NOT OUR PIN. Reading every `rev =`
# in the file traced a third-party git dependency as though it pinned
# this repository -- and, its object being absent, the run exited 2 with
# a shallow-clone diagnosis: a hard stop with the wrong cause.
new_provenance_sandbox yes yes
cat >> "$SANDBOX/spikes/spike-test/harness/Cargo.toml" <<'EOF'

[package.metadata.foreign]
elsewhere = { git = "https://example.invalid/other/thing.git", rev = "0123456789abcdef0123456789abcdef01234567" }
EOF
run_provenance_guard
assert_rc "a foreign repository's rev is not traced as ours" 0
if [[ "$RUN_OUT" != *"0123456789abcdef"* ]]; then
    pass "and is not named in the output"
else
    fail "and is not named in the output" "$RUN_OUT"
fi
rm -rf "$SANDBOX"; SANDBOX=""

# A COMMENTED-OUT GIT DEPENDENCY IS NOT A DEPENDENCY. The gap check used
# to grep the RAW manifest for `git = ...InterWeave` while looking for a
# rev only in the comment-stripped text, so a manifest that had commented
# its git dependency out -- which these manifests discuss doing -- was
# reported as pinning this repository with no readable revision.
new_provenance_sandbox yes yes
cat >> "$SANDBOX/spikes/spike-test/harness/Cargo.toml" <<'EOF'

# commented = { git = "https://github.com/gzapi-org/InterWeave.git" }
EOF
run_provenance_guard
assert_rc "a commented-out git dependency raises nothing" 0
if [[ "$RUN_OUT" != *"no revision this check can read"* ]]; then
    pass "and is not reported as an unreadable pin"
else
    fail "and is not reported as an unreadable pin" "$RUN_OUT"
fi
rm -rf "$SANDBOX"; SANDBOX=""

if (( failures > 0 )); then
    echo "test_check_spike_locks: $failures assertion(s) failed." >&2
    exit 1
fi
echo "test_check_spike_locks: OK — all assertions passed."
