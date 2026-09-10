#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# Self-test for check_vendored_advisories.sh.
#
# The case that matters is the POSITIVE one: a vendored crate carrying a
# RustSec advisory must fail. `atty 0.2.14` is the fixture, because it is
# the crate the guard's own help cites as the measurement — it carries
# RUSTSEC-2021-0145 and RUSTSEC-2024-0375, and path-patching it is what
# made `cargo deny check advisories` fall silent.
#
# Each sandbox is a real resolvable workspace: the guard asks
# `cargo metadata --locked`, so a fixture needs a lockfile, and cargo
# needs a target, so the vendored directory needs a source file even
# though nothing compiles it.
#
# The guard checks for `cargo-deny` before it does anything else and
# exits 2 without it, so EVERY case here needs it -- this self-test
# skips whole rather than degrading to a subset, which an earlier
# revision of this comment got wrong.

set -uo pipefail

ROOT="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )/../.." && pwd )"
GUARD="$ROOT/tools/checks/check_vendored_advisories.sh"

failures=0
ok()   { printf '  \xe2\x9c\x93 %s\n' "$1"; }
bad()  { printf '  \xe2\x9c\x97 %s\n' "$1"; failures=$((failures + 1)); }

SANDBOX="$(mktemp -d)" || { echo "cannot create a sandbox" >&2; exit 1; }
trap 'rm -rf "$SANDBOX"' EXIT

# A SKIP IS A FAILURE IN CI. Locally, missing cargo-deny or an offline
# registry is an ordinary state and skipping is right. In CI both are
# installed on purpose, so a skip there means the job stopped exercising
# the guard -- the shape `test_check_dependencies.sh`'s comment in
# ci.yml exists to prevent. Review finding on PR #85.
skip_or_fail() {
    if [ -n "${CI:-}" ]; then
        printf 'test_check_vendored_advisories: %s — and this is CI, where it\n' "$1" >&2
        printf 'is installed on purpose, so the suite is not exercising the guard.\n' >&2
        exit 1
    fi
    printf 'test_check_vendored_advisories: %s — skipped whole.\n' "$1"
    # ASSERTIONS ALREADY RECORDED STILL FAIL THE SUITE. Exiting 0 here made
    # a skip late in the file mask every `✗` printed before it -- and the
    # last call site is the final fixture, where the masking window is the
    # whole suite. CI is protected by the branch above, so this was a
    # local-verification hole; `cargo xtask ci` would have reported green
    # with failures on screen. Review finding on PR #85.
    [ "$failures" -eq 0 ] || {
        printf 'but %d assertion(s) had already failed, so this is not a pass.\n' \
            "$failures" >&2
        exit 1
    }
    exit 0
}

if ! cargo deny --version >/dev/null 2>&1; then
    skip_or_fail "cargo-deny is absent"
fi

# Is the advisory database reachable at all? Everything below distinguishes
# a finding from an environment failure, and without this baseline it
# could not: a guard mutation that misclassifies findings as unreachable
# would otherwise look like a skip and report success.
mkdir -p "$SANDBOX/baseline/src"
cat > "$SANDBOX/baseline/Cargo.toml" <<'EOF'
[package]
name = "baseline"
version = "0.0.0"
edition = "2021"

[dependencies]
atty = "=0.2.14"
EOF
echo 'fn main() {}' > "$SANDBOX/baseline/src/main.rs"
cp "$ROOT/deny.toml" "$SANDBOX/baseline/deny.toml"
if (cd "$SANDBOX/baseline" && cargo generate-lockfile >/dev/null 2>&1); then
    if (cd "$SANDBOX/baseline" && cargo deny check advisories >/dev/null 2>&1); then
        skip_or_fail "atty 0.2.14 reports no advisory, so the database is stale or it was cleared"
    fi
else
    skip_or_fail "the registry is unreachable"
fi

# A workspace with no [patch.crates-io] block at all.
mkdir -p "$SANDBOX/none/src"
cat > "$SANDBOX/none/Cargo.toml" <<'EOF'
[package]
name = "nothing-vendored"
version = "0.0.0"
edition = "2021"
EOF
echo 'fn main() {}' > "$SANDBOX/none/src/main.rs"
cp "$ROOT/deny.toml" "$SANDBOX/none/deny.toml"
(cd "$SANDBOX/none" && cargo generate-lockfile >/dev/null 2>&1) \
    || { echo "cannot resolve the empty fixture" >&2; exit 1; }

if bash "$GUARD" --root "$SANDBOX/none" >/dev/null 2>&1; then
    ok "a workspace vendoring nothing passes"
else
    bad "a workspace vendoring nothing must pass"
fi

# Build a workspace that vendors one crate. $1 sandbox name, $2 crate,
# $3 version, $4 the [patch.crates-io] spelling (inline or subtable).
# $5 replaces the [dependencies] block when given; $6 is appended after
# the patch table. Both default to the ordinary shape.
build_vendored() {
    local dir="$SANDBOX/$1" crate="$2" ver="$3" form="$4"
    mkdir -p "$dir/src" "$dir/third_party/$crate/src"
    {
        printf '[package]\nname = "%s-probe"\nversion = "0.0.0"\nedition = "2021"\n\n' "$1"
        if [ -n "${5:-}" ]; then
            printf '%s\n\n' "$5"
        else
            printf '[dependencies]\n%s = "=%s"\n\n' "$crate" "$ver"
        fi
        if [ "$form" = subtable ]; then
            printf '[patch.crates-io.%s]\npath = "third_party/%s"\n' "$crate" "$crate"
        else
            printf '[patch.crates-io]\n%s = { path = "third_party/%s" }\n' "$crate" "$crate"
        fi
        [ -n "${6:-}" ] && printf '\n%s\n' "$6"
    } > "$dir/Cargo.toml"
    echo 'fn main() {}' > "$dir/src/main.rs"
    printf '[package]\nname = "%s"\nversion = "%s"\nedition = "2018"\n' "$crate" "$ver" \
        > "$dir/third_party/$crate/Cargo.toml"
    echo '' > "$dir/third_party/$crate/src/lib.rs"
    cp "$ROOT/deny.toml" "$dir/deny.toml"
    (cd "$dir" && cargo generate-lockfile >/dev/null 2>&1) \
        || { echo "cannot resolve the $1 fixture" >&2; exit 1; }
}

# Run the guard and classify. Exit 2 is only ever an environment problem,
# and the baseline above proved the environment works -- so here it is a
# FAILURE, not a skip. Without that, a mutation misclassifying findings as
# unreachable would report success.
expect_finding() {
    local dir="$1" label="$2" id="$3"
    local out status
    out="$(bash "$GUARD" --root "$SANDBOX/$dir" 2>&1)"; status=$?
    case "$status" in
        1) ok "$label" ;;
        2) bad "$label — exit 2, but the baseline proved the database reachable" ;;
        *) bad "$label — expected exit 1, got $status" ;;
    esac
    if printf '%s' "$out" | grep -q "$id"; then
        ok "  and $id is reported"
    else
        bad "  $id must appear in the output"
    fi
}

# A VENDORED CRATE THAT IS ACTUALLY CLEAN PASSES THROUGH THE PROBE.
#
# Every other exit-0 assertion in this suite BYPASSES the probe loop: the
# `none` fixture above vendors nothing, and `ours` is skipped as
# first-party. So the guard's success path -- `checked=$((checked + 1))`
# and the summary line that reports it -- had no assertion at all, and both
# could be deleted with the whole suite green. This is the only fixture
# that makes cargo-deny run, find nothing, and report it. Review finding on
# PR #85.
#
# `cfg-if` because it is tiny, stable and carries no RustSec advisory at
# 1.0.0; if that ever stops being true this fixture fails LOUDLY with the
# advisory named, which is the right way round -- and it is a
# VERSION-DATED FIXTURE in the opposite polarity to the four advisory ids
# the guard's help lists, which is why that paragraph names it too.
#
# This block used to sit between the stale-lockfile paragraph below and the
# code that paragraph describes, joined by an "AND" that made two unrelated
# cases read as one. Moved above it instead. Review findings on PR #85.
build_vendored clean cfg-if 1.0.0 inline
out="$(bash "$GUARD" --root "$SANDBOX/clean" 2>&1)"; status=$?
case $status in
    0) ok "a clean vendored crate passes through the probe" ;;
    1) bad "the clean fixture reported an advisory — $out" ;;
    *) bad "a clean vendored crate — expected exit 0, got $status: $out" ;;
esac
# THE COUNT, not only the exit code. Without this, `checked` can be frozen
# at zero and nothing notices.
if printf '%s' "$out" | grep -q '1 vendored crate(s) free of RustSec advisories'; then
    ok "  and the summary counts the tree it checked"
else
    bad "  the success summary must report one checked tree: $out"
fi

# A LOCKFILE THAT DOES NOT SATISFY THE MANIFEST is exit 2, not a silent
# re-resolve: the guard promises to leave the working tree alone, and an
# earlier version's `--locked` fallback rewrote `Cargo.lock`.
build_vendored stale atty 0.2.14 inline
rm -f "$SANDBOX/stale/Cargo.lock"
bash "$GUARD" --root "$SANDBOX/stale" >/dev/null 2>&1
case $? in
    2) ok "a lockfile that cannot be used is exit 2" ;;
    *) bad "a missing lockfile must exit 2" ;;
esac
if [ -f "$SANDBOX/stale/Cargo.lock" ]; then
    bad "the guard must not write a lockfile into the tree it checks"
else
    ok "  and no lockfile is written behind it"
fi

# CARGO-DENY THAT NEVER RAN must be exit 2, not a pass. An earlier
# version classified FAILURES by matching network wording, so any other
# failure -- here a `deny.toml` cargo-deny cannot deserialize -- fell
# through to a filter that found no advisory and called the crate clean.
# The crate in this fixture genuinely carries two advisories, so a pass
# here is a false pass and not merely a missed error.
build_vendored unrunnable atty 0.2.14 inline
printf '[advisories]\nthis-key-does-not-exist = "boom"\nversion = 2\n' \
    > "$SANDBOX/unrunnable/deny.toml"
bash "$GUARD" --root "$SANDBOX/unrunnable" >/dev/null 2>&1
case $? in
    2) ok "cargo-deny that could not run is exit 2, not a pass" ;;
    1) bad "exit 1 — it reported a finding it cannot have obtained" ;;
    *) bad "a cargo-deny that never ran must exit 2, got $?" ;;
esac

# THE POSITIVE CASE.
build_vendored vulnerable atty 0.2.14 inline
expect_finding vulnerable "a vendored crate carrying an advisory fails" RUSTSEC-2021-0145

# AN ADVISORY WHOSE OWN TEXT MATCHES THE NETWORK WORDING must be reported
# as the finding it is. `rand 0.9.0` carries RUSTSEC-2026-0097, whose
# description contains "unable to" -- an earlier discriminator grepped
# the whole output, advisory bodies included, and called it an
# unreachable database, which tells an operator to re-run rather than to
# act. Review finding on PR #85.
build_vendored textmatch rand 0.9.0 inline
expect_finding textmatch "an advisory reading like a network error is still a finding" RUSTSEC-2026-0097

# A VENDORED TREE THE GRAPH DOES NOT EXPLAIN is exit 2, never "nothing is
# vendored". Each of these ships a real vulnerable tree and produced a
# confident OK with exit 0 before the floor existed.
expect_unexplained() {
    bash "$GUARD" --root "$SANDBOX/$1" >/dev/null 2>&1
    case $? in
        2) ok "$2" ;;
        0) bad "$2 -- reported success for a tree it did not check" ;;
        *) bad "$2 -- expected exit 2, got a different code" ;;
    esac
}

build_vendored unused atty 0.2.14 inline '# nothing depends on it'
expect_unexplained unused "a patch nothing uses is not 'nothing is vendored'"

# A VENDORED CRATE LISTED AS A WORKSPACE MEMBER is CHECKED, not skipped.
# ADR-0051 deliberately keeps the vendored crate OUT of `members`, so this
# is not the expected layout -- but a selection keyed on "not a workspace
# member" turns the guard off for anyone who puts it in, and silence is
# the wrong answer to an unexpected layout. Review findings on PR #85.
build_vendored amember atty 0.2.14 inline '[dependencies]
atty = "=0.2.14"' '[workspace]
members = ["third_party/atty"]'
expect_finding amember "a vendored crate listed as a workspace member is still checked" RUSTSEC-2021-0145

# EVERY TREE ACCOUNTED FOR, IN BOTH DIRECTIONS. The graph says what cargo
# builds from a local tree; the disk says what this repository ships. A
# resolvable path crate elsewhere must not stand in for a vendored tree
# the graph never named -- and the stand-in has to be RESOLVABLE, because
# an earlier fixture used an unpublished helper and so passed under the
# count logic for an unrelated reason: that guard died resolving the
# helper before it reached the floor at all. Review findings on PR #85.
offset="$SANDBOX/offset"
mkdir -p "$offset/src" "$offset/third_party/localonly/src" "$offset/elsewhere/atty/src"
cat > "$offset/Cargo.toml" <<'OFFSET'
[package]
name = "offset-probe"
version = "0.0.0"
edition = "2021"

[dependencies]
atty = "=0.2.14"

[patch.crates-io]
atty = { path = "elsewhere/atty" }
OFFSET
echo 'fn main() {}' > "$offset/src/main.rs"
printf '[package]\nname = "atty"\nversion = "0.2.14"\nedition = "2018"\n' \
    > "$offset/elsewhere/atty/Cargo.toml"
echo '' > "$offset/elsewhere/atty/src/lib.rs"
printf '[package]\nname = "localonly"\nversion = "0.0.0"\nedition = "2021"\n' \
    > "$offset/third_party/localonly/Cargo.toml"
echo '' > "$offset/third_party/localonly/src/lib.rs"
cp "$ROOT/deny.toml" "$offset/deny.toml"
(cd "$offset" && cargo generate-lockfile >/dev/null 2>&1) \
    || { echo "cannot resolve the offset fixture" >&2; exit 1; }
expect_unexplained offset "a shipped tree absent from the graph is exit 2 even when another is checked"

# A VENDORED TREE NESTED DEEPER THAN ONE LEVEL, which a disk glob of
# `third_party/*/Cargo.toml` could not see -- and which is the shape you
# get from vendoring a subtree of a multi-crate upstream repository.
nested="$SANDBOX/nested"
mkdir -p "$nested/src" "$nested/third_party/upstream/atty/src"
cat > "$nested/Cargo.toml" <<'NESTED'
[package]
name = "nested-probe"
version = "0.0.0"
edition = "2021"

[dependencies]
atty = "=0.2.14"

[patch.crates-io]
atty = { path = "third_party/upstream/atty" }
NESTED
echo 'fn main() {}' > "$nested/src/main.rs"
printf '[package]\nname = "atty"\nversion = "0.2.14"\nedition = "2018"\n' \
    > "$nested/third_party/upstream/atty/Cargo.toml"
echo '' > "$nested/third_party/upstream/atty/src/lib.rs"
cp "$ROOT/deny.toml" "$nested/deny.toml"
(cd "$nested" && cargo generate-lockfile >/dev/null 2>&1) \
    || { echo "cannot resolve the nested fixture" >&2; exit 1; }
expect_finding nested "a tree nested deeper than one level is still checked" RUSTSEC-2021-0145

# A PATCH PATH OUTSIDE `third_party/`, which ADR-0051 Decision 7 promises
# coverage of and a disk-only scan could not see.
outside="$SANDBOX/outside"
mkdir -p "$outside/src" "$outside/vendor/atty/src"
cat > "$outside/Cargo.toml" <<'OUTSIDE'
[package]
name = "outside-probe"
version = "0.0.0"
edition = "2021"

[dependencies]
atty = "=0.2.14"

[patch.crates-io]
atty = { path = "vendor/atty" }
OUTSIDE
echo 'fn main() {}' > "$outside/src/main.rs"
printf '[package]\nname = "atty"\nversion = "0.2.14"\nedition = "2018"\n' \
    > "$outside/vendor/atty/Cargo.toml"
echo '' > "$outside/vendor/atty/src/lib.rs"
cp "$ROOT/deny.toml" "$outside/deny.toml"
(cd "$outside" && cargo generate-lockfile >/dev/null 2>&1) \
    || { echo "cannot resolve the outside fixture" >&2; exit 1; }
expect_finding outside "a patch path outside third_party is checked too" RUSTSEC-2021-0145

# AN UNUSED PATCH PATH OUTSIDE `third_party/` is in NEITHER the graph nor
# the disk scan: cargo omits an unused patch from the graph entirely, and
# the disk scan only walks `third_party/`. The tree still ships and a
# dependency bump re-arms it, so "nothing is built from a local tree" is
# the wrong answer. Review finding on PR #85.
unusedout="$SANDBOX/unusedout"
mkdir -p "$unusedout/src" "$unusedout/vendor/atty/src"
cat > "$unusedout/Cargo.toml" <<'UNUSEDOUT'
[package]
name = "unusedout-probe"
version = "0.0.0"
edition = "2021"

[patch.crates-io]
atty = { path = "vendor/atty" }
UNUSEDOUT
echo 'fn main() {}' > "$unusedout/src/main.rs"
printf '[package]\nname = "atty"\nversion = "0.2.14"\nedition = "2018"\n' \
    > "$unusedout/vendor/atty/Cargo.toml"
echo '' > "$unusedout/vendor/atty/src/lib.rs"
cp "$ROOT/deny.toml" "$unusedout/deny.toml"
(cd "$unusedout" && cargo generate-lockfile >/dev/null 2>&1) \
    || { echo "cannot resolve the unusedout fixture" >&2; exit 1; }
expect_unexplained unusedout "an unused patch path outside third_party is not 'nothing is vendored'"

# THE SAME PATCH DECLARED IN CARGO'S CONFIGURATION rather than the
# manifest. The Cargo reference says a `[patch]` table may live in
# `.cargo/config.toml`, and cargo omits an unused one from the graph
# wherever it was declared -- so reading only the manifest left this
# invisible. Review finding on PR #85.
cfgpatch="$SANDBOX/cfgpatch"
mkdir -p "$cfgpatch/src" "$cfgpatch/vendor/atty/src" "$cfgpatch/.cargo"
printf '[package]\nname = "cfgpatch-probe"\nversion = "0.0.0"\nedition = "2021"\n' \
    > "$cfgpatch/Cargo.toml"
printf '[patch.crates-io]\natty = { path = "vendor/atty" }\n' > "$cfgpatch/.cargo/config.toml"
echo 'fn main() {}' > "$cfgpatch/src/main.rs"
printf '[package]\nname = "atty"\nversion = "0.2.14"\nedition = "2018"\n' \
    > "$cfgpatch/vendor/atty/Cargo.toml"
echo '' > "$cfgpatch/vendor/atty/src/lib.rs"
cp "$ROOT/deny.toml" "$cfgpatch/deny.toml"
(cd "$cfgpatch" && cargo generate-lockfile >/dev/null 2>&1) \
    || { echo "cannot resolve the cfgpatch fixture" >&2; exit 1; }
expect_unexplained cfgpatch "a patch declared in .cargo/config.toml is not missed"

# THE LEGACY SPELLING, `.cargo/config` without the extension, which cargo
# still reads and which the commit that added config support also added --
# untested until a reviewer said so. Review finding on PR #85.
oldcfg="$SANDBOX/oldcfg"
mkdir -p "$oldcfg/src" "$oldcfg/vendor/atty/src" "$oldcfg/.cargo"
printf '[package]\nname = "oldcfg-probe"\nversion = "0.0.0"\nedition = "2021"\n' \
    > "$oldcfg/Cargo.toml"
printf '[patch.crates-io]\natty = { path = "vendor/atty" }\n' > "$oldcfg/.cargo/config"
echo 'fn main() {}' > "$oldcfg/src/main.rs"
printf '[package]\nname = "atty"\nversion = "0.2.14"\nedition = "2018"\n' \
    > "$oldcfg/vendor/atty/Cargo.toml"
echo '' > "$oldcfg/vendor/atty/src/lib.rs"
cp "$ROOT/deny.toml" "$oldcfg/deny.toml"
(cd "$oldcfg" && cargo generate-lockfile >/dev/null 2>&1) \
    || { echo "cannot resolve the oldcfg fixture" >&2; exit 1; }
expect_unexplained oldcfg "the legacy .cargo/config spelling is not missed"

# A CONFIG CARGO READS AND `tomllib` WOULD NOT: a UTF-8 BOM. The failure
# was unobservable, because a non-zero exit inside a process substitution
# is swallowed by `mapfile` -- so the patch table went unread while the
# graph still resolved. Both halves are fixed: the BOM is tolerated, and a
# table that genuinely cannot be read is exit 2. Review finding on PR #85.
bomcfg="$SANDBOX/bomcfg"
mkdir -p "$bomcfg/src" "$bomcfg/vendor/atty/src" "$bomcfg/.cargo"
printf '[package]\nname = "bomcfg-probe"\nversion = "0.0.0"\nedition = "2021"\n' \
    > "$bomcfg/Cargo.toml"
printf '\xef\xbb\xbf[patch.crates-io]\natty = { path = "vendor/atty" }\n' \
    > "$bomcfg/.cargo/config.toml"
echo 'fn main() {}' > "$bomcfg/src/main.rs"
printf '[package]\nname = "atty"\nversion = "0.2.14"\nedition = "2018"\n' \
    > "$bomcfg/vendor/atty/Cargo.toml"
echo '' > "$bomcfg/vendor/atty/src/lib.rs"
cp "$ROOT/deny.toml" "$bomcfg/deny.toml"
(cd "$bomcfg" && cargo generate-lockfile >/dev/null 2>&1) \
    || { echo "cannot resolve the bomcfg fixture" >&2; exit 1; }
# THE EXIT CODE ALONE CANNOT TELL THE TWO OUTCOMES APART, which is why the
# stderr is read here. Tolerating the BOM parses the table, so `vendor/atty`
# joins the shipped set, is an unused patch, and lands at the ACCOUNTING
# floor; refusing it raises `TOMLDecodeError` and lands at "cannot read a
# Cargo patch table". Both are exit 2, so `expect_unexplained` on its own
# asserted only the half that was never in doubt. Review finding on PR #85.
out="$(bash "$GUARD" --root "$bomcfg" 2>&1)"; status=$?
case $status in
    2) ok "a config carrying a BOM is read rather than silently skipped" ;;
    0) bad "a BOM'd config was skipped and the tree went unasked — exit 0" ;;
    *) bad "a BOM'd config — expected exit 2, got $status" ;;
esac
if printf '%s' "$out" | grep -q 'not in the package graph'; then
    ok "  and it is the accounting floor that refuses, so the BOM was tolerated"
else
    bad "  a tolerated BOM must reach the accounting floor, not the unreadable-table path: $out"
fi

# A TREE WITH NO REGISTRY RELEASE is an INCOMPLETE sweep, not a clean one:
# it passes the graph floor, so the guard reaches it and cannot ask about
# it. `deny.toml` bars git dependencies, so vendoring is the sanctioned
# answer for such a crate and this path is reachable by design -- and it
# was reached by no fixture. Review finding on PR #85.
norelease="$SANDBOX/norelease"
mkdir -p "$norelease/src" "$norelease/third_party/interweave-not-published/src"
cat > "$norelease/Cargo.toml" <<'NORELEASE'
[package]
name = "norelease-probe"
version = "0.0.0"
edition = "2021"

[dependencies]
interweave-not-published = { path = "third_party/interweave-not-published" }
NORELEASE
echo 'fn main() {}' > "$norelease/src/main.rs"
printf '[package]\nname = "interweave-not-published"\nversion = "0.0.0"\nedition = "2021"\n' \
    > "$norelease/third_party/interweave-not-published/Cargo.toml"
echo '' > "$norelease/third_party/interweave-not-published/src/lib.rs"
cp "$ROOT/deny.toml" "$norelease/deny.toml"
(cd "$norelease" && cargo generate-lockfile >/dev/null 2>&1) \
    || { echo "cannot resolve the norelease fixture" >&2; exit 1; }
out="$(bash "$GUARD" --root "$norelease" 2>&1)"
case $? in
    2) ok "a tree with no registry release makes the sweep incomplete, not clean" ;;
    0) bad "an unaskable tree must not report success" ;;
    *) bad "an unaskable tree must exit 2" ;;
esac
if printf '%s' "$out" | grep -q 'could not be resolved from the'; then
    ok "  and says it could not ask rather than naming a cause it does not know"
else
    bad "  the summary must say the tree could not be resolved"
fi

# AN IGNORE ON ONE ADVISORY MUST NOT SILENCE ANOTHER. This fixture sets
# `ignore` on one id and asserts the second is still reported, which is the
# ignore-list interaction and nothing more.
#
# It is NOT a test of the severity filter's `warning` arm, which an earlier
# version of this header claimed. That arm is a forward guard with no
# fixture, and the reason is stated where the filter is -- not "per-class
# levels are gone", which `deny.toml`'s own `yanked` key disproves, but that
# a diagnostic carrying no `advisory` object is dropped regardless of its
# severity. Review finding on PR #85.
warned="$SANDBOX/warned"
build_vendored warned atty 0.2.14 inline
printf '[advisories]\nversion = 2\nunmaintained = "workspace"\nyanked = "warn"\nignore = ["RUSTSEC-2021-0145"]\n' \
    > "$warned/deny.toml"
out="$(bash "$GUARD" --root "$warned" 2>&1)"
status=$?
if printf '%s' "$out" | grep -q 'RUSTSEC-2024-0375'; then
    ok "the remaining advisory is still reported when another is ignored"
else
    bad "an advisory outside the ignore list must still be reported"
fi
case $status in
    1) ok "  and the crate still fails" ;;
    *) bad "  a vendored crate with a live advisory must exit 1" ;;
esac
# AND THE IGNORED ONE IS GONE, which is what the help's "any documented
# ignore still applies" means and what nothing asserted.
#
# WHAT THIS PINS IS THE INHERITANCE, not the severity filter. A reviewer
# proposed it as cover for that filter, on the reasoning that an ignored
# advisory arrives at note level and the filter is what drops it.
# MEASURED, AND THAT IS NOT THE MECHANISM: `cargo deny` with the id on its
# `ignore` list emits no record mentioning the id at all, so removing the
# severity filter leaves this assertion green -- checked by planting
# exactly that. What DOES kill it is the probe not inheriting this
# repository's `deny.toml`, which is the claim the help actually makes:
# replacing the `cp deny.toml` with a bare `[advisories]` stub makes the
# ignored advisory surface and this line fail. Review finding on PR #85,
# and a correction to the finding.
if printf '%s' "$out" | grep -q 'RUSTSEC-2021-0145'; then
    bad "  an advisory on deny.toml's ignore list must not be reported"
else
    ok "  and the ignored advisory is absent, so the ignore list is honoured"
fi

# `--root` ON A MISSING DIRECTORY must say so and stop. `cd "$ROOT" || die`
# ran while `die` was still undefined, and with no `set -e` the script
# carried on in the caller's directory -- which could report a verdict for
# the wrong repository. Review finding on PR #85.
out="$(bash "$GUARD" --root "$SANDBOX/does-not-exist" 2>&1)"
case $? in
    2) ok "a missing --root is exit 2" ;;
    *) bad "a missing --root must exit 2" ;;
esac
if printf '%s' "$out" | grep -q 'check_vendored_advisories: cannot enter'; then
    ok "  and it names itself rather than leaving bash to explain"
else
    bad "  the refusal must carry the guard's own message"
fi

# A CRATE BEHIND AN OPTIONAL FEATURE is resolved, because `--all-features`
# is what `deny.toml` itself uses, and CLAUDE.md §1 records that the
# connectivity behaviours ship gated off -- one manifest edit from this.
build_vendored optional atty 0.2.14 inline '[dependencies]
atty = { version = "=0.2.14", optional = true }'
expect_finding optional "a crate behind an optional feature is still checked" RUSTSEC-2021-0145

# AN UNKNOWN ARGUMENT must not silently check this repository instead of
# the tree the caller named.
bash "$GUARD" -root /nonexistent >/dev/null 2>&1
case $? in
    1) ok "an unrecognised argument is refused" ;;
    *) bad "an unrecognised argument must exit 1" ;;
esac

# THE SUB-TABLE FORM, which cargo accepts identically. An awk-based
# parser saw nothing here and printed "no crate is vendored", exit 0 --
# a false pass in the guard that is the only warning there is.
build_vendored subtable atty 0.2.14 subtable
expect_finding subtable "the [patch.crates-io.<crate>] form is not missed" RUSTSEC-2021-0145

# THE VERSION PIN. `time 0.1.45` carries RUSTSEC-2020-0071; 0.3.x does
# not. A guard that asked the registry for the crate without pinning the
# VENDORED version would resolve the clean release and pass.
build_vendored pinned time 0.1.45 inline
expect_finding pinned "the vendored VERSION is what gets asked about" RUSTSEC-2020-0071

# THE REASON THIS GUARD EXISTS: cargo-deny alone must still miss it. If
# that ever stops being true this guard may be redundant, and the fixture
# is what will say so rather than it quietly becoming dead weight.
if (cd "$SANDBOX/vulnerable" && cargo generate-lockfile >/dev/null 2>&1); then
    if (cd "$SANDBOX/vulnerable" && cargo deny check advisories >/dev/null 2>&1); then
        ok "cargo-deny alone still misses a path-patched crate"
    else
        bad "cargo-deny now reports path-patched crates — check before deleting this guard"
    fi
else
    bad "the vulnerable fixture must resolve; it is the basis of every case above"
fi

# A CRATE VENDORED OUTSIDE third_party/ IS STILL VENDORED. Cargo promotes
# every path dependency inside the workspace directory to a member, so the
# old "a member is first-party" test skipped this layout -- and the disk
# scan walks third_party/ only, and the patch table is not involved, so it
# appeared in none of the three sources. The guard printed "nothing is
# built from a local tree" and exited 0 with a vulnerable crate compiled
# in. Review finding on PR #85.
#
# AN EXPLICIT [workspace] TABLE IS LOAD-BEARING IN THIS FIXTURE, and the
# first version of it got that wrong. A single-package manifest does NOT
# promote its path dependencies to members -- measured: `workspace_members`
# holds the root package only -- so the fixture passed against the very
# logic it was written to catch. A workspace ROOT does promote them, even
# ones `members` does not list, which is the real repository layout and the
# shape the finding was about.
elsewhere="$SANDBOX/elsewhere"
# `apps`, not `app`: the scaffolding package must sit in a real landing
# zone, or it is itself selected as vendored and the assertion below passes
# for a second reason. Review finding on PR #85.
mkdir -p "$elsewhere/apps/probe/src" "$elsewhere/vendor/atty/src"
printf '[workspace]\nmembers = ["apps/probe"]\nresolver = "2"\n' > "$elsewhere/Cargo.toml"
{
    printf '[package]\nname = "elsewhere-probe"\nversion = "0.0.0"\nedition = "2021"\n\n'
    printf '[dependencies]\natty = { path = "../../vendor/atty" }\n'
} > "$elsewhere/apps/probe/Cargo.toml"
echo 'fn main() {}' > "$elsewhere/apps/probe/src/main.rs"
printf '[package]\nname = "atty"\nversion = "0.2.14"\nedition = "2018"\n' \
    > "$elsewhere/vendor/atty/Cargo.toml"
echo '' > "$elsewhere/vendor/atty/src/lib.rs"
cp "$ROOT/deny.toml" "$elsewhere/deny.toml"
if (cd "$elsewhere" && cargo generate-lockfile >/dev/null 2>&1); then
    out="$(bash "$GUARD" --root "$elsewhere" 2>&1)"; status=$?
    case "$status" in
        1) ok "a path dependency outside third_party/ is still asked about" ;;
        0) bad "a vendored crate outside third_party/ was skipped entirely — exit 0" ;;
        2) bad "a path dependency outside third_party/ — exit 2, environment" ;;
        *) bad "a path dependency outside third_party/ — expected exit 1, got $status" ;;
    esac
    if printf '%s' "$out" | grep -q 'RUSTSEC-2021-0145'; then
        ok "  and its advisory is reported"
    else
        bad "  the advisory of a crate outside third_party/ must be reported"
    fi
else
    skip_or_fail "the elsewhere fixture cannot resolve"
fi

# AND A FIRST-PARTY CRATE IS STILL NOT PROBED. The rule above is a
# location test, so the control is a local path dependency that IS in a
# landing zone: it must be treated as ours and not sent to the registry.
ours="$SANDBOX/ours"
mkdir -p "$ours/apps/probe/src" "$ours/crates/atty/src"
printf '[workspace]\nmembers = ["apps/probe"]\nresolver = "2"\n' > "$ours/Cargo.toml"
{
    printf '[package]\nname = "ours-probe"\nversion = "0.0.0"\nedition = "2021"\n\n'
    printf '[dependencies]\natty = { path = "../../crates/atty" }\n'
} > "$ours/apps/probe/Cargo.toml"
echo 'fn main() {}' > "$ours/apps/probe/src/main.rs"
printf '[package]\nname = "atty"\nversion = "0.2.14"\nedition = "2018"\n' \
    > "$ours/crates/atty/Cargo.toml"
echo '' > "$ours/crates/atty/src/lib.rs"
cp "$ROOT/deny.toml" "$ours/deny.toml"
if (cd "$ours" && cargo generate-lockfile >/dev/null 2>&1); then
    out="$(bash "$GUARD" --root "$ours" 2>&1)"; status=$?
    if [ "$status" -eq 0 ]; then
        ok "a path dependency under crates/ is treated as first-party"
    else
        bad "a first-party crate must not be probed: exit $status"
    fi
    if printf '%s' "$out" | grep -q 'RUSTSEC-2021-0145'; then
        bad "  and its name must not be sent to the registry as a vendored crate"
    else
        ok "  and no advisory is attributed to it"
    fi
else
    skip_or_fail "the ours fixture cannot resolve"
fi

# --root WITH NO VALUE names itself rather than leaving bash to explain,
# which the adjacent missing-argument case already asserts for its own
# shape. Review finding on PR #85.
out="$(bash "$GUARD" --root 2>&1)"; status=$?
if [ "$status" -eq 2 ]; then
    ok "--root with no value is exit 2"
else
    bad "--root with no value must be exit 2, got $status"
fi
if printf '%s' "$out" | grep -q 'check_vendored_advisories: --root needs a directory'; then
    ok "  and the guard names itself"
else
    bad "  the diagnostic must come from the guard, not from bash: $out"
fi

# AND A TREE EXCLUDED FROM THE WORKSPACE IS STILL VENDORED. `exclude` is
# the documented way to keep a path dependency inside the workspace
# directory out of `members`, so a location-only test skipped a tree
# vendored under a landing zone -- the same exit-0 verdict as the
# membership-only test, one directory over. Review finding on PR #85.
excluded="$SANDBOX/excluded"
mkdir -p "$excluded/apps/probe/src" "$excluded/crates/vendored-atty/src"
printf '[workspace]\nmembers = ["apps/probe"]\nexclude = ["crates/vendored-atty"]\nresolver = "2"\n' \
    > "$excluded/Cargo.toml"
{
    printf '[package]\nname = "excluded-probe"\nversion = "0.0.0"\nedition = "2021"\n\n'
    printf '[dependencies]\natty = { path = "../../crates/vendored-atty" }\n'
} > "$excluded/apps/probe/Cargo.toml"
echo 'fn main() {}' > "$excluded/apps/probe/src/main.rs"
printf '[package]\nname = "atty"\nversion = "0.2.14"\nedition = "2018"\n' \
    > "$excluded/crates/vendored-atty/Cargo.toml"
echo '' > "$excluded/crates/vendored-atty/src/lib.rs"
cp "$ROOT/deny.toml" "$excluded/deny.toml"
if (cd "$excluded" && cargo generate-lockfile >/dev/null 2>&1); then
    out="$(bash "$GUARD" --root "$excluded" 2>&1)"; status=$?
    case "$status" in
        1) ok "a tree excluded from the workspace is still asked about" ;;
        0) bad "an excluded vendored tree under a landing zone was skipped — exit 0" ;;
        *) bad "an excluded vendored tree — expected exit 1, got $status" ;;
    esac
    if printf '%s' "$out" | grep -q 'RUSTSEC-2021-0145'; then
        ok "  and its advisory is reported"
    else
        bad "  the advisory of an excluded vendored tree must be reported"
    fi
else
    skip_or_fail "the excluded fixture cannot resolve"
fi

# TWO VENDORED TREES OF ONE NAME each get their own probe directory, which
# is what the row numbering is for.
twins="$SANDBOX/twins"
mkdir -p "$twins/apps/probe/src" "$twins/third_party/a/src" "$twins/third_party/b/src"
printf '[workspace]\nmembers = ["apps/probe"]\nresolver = "2"\n' > "$twins/Cargo.toml"
{
    printf '[package]\nname = "twins-probe"\nversion = "0.0.0"\nedition = "2021"\n\n'
    printf '[dependencies]\natty = { path = "../../third_party/a" }\n'
} > "$twins/apps/probe/Cargo.toml"
echo 'fn main() {}' > "$twins/apps/probe/src/main.rs"
for leaf in a b; do
    printf '[package]\nname = "atty"\nversion = "0.2.14"\nedition = "2018"\n' \
        > "$twins/third_party/$leaf/Cargo.toml"
    echo '' > "$twins/third_party/$leaf/src/lib.rs"
done
cp "$ROOT/deny.toml" "$twins/deny.toml"
if (cd "$twins" && cargo generate-lockfile >/dev/null 2>&1); then
    out="$(bash "$GUARD" --root "$twins" 2>&1)"; status=$?
    # `b` is shipped but unreachable from the graph, so the mutual
    # accounting floor refuses -- which is the documented behaviour and is
    # exit 2. WHAT THIS PINS is only that both trees are NAMED by the
    # floor, so neither is silently dropped. It does NOT pin the per-row
    # probe directory: the refusal happens before the probe loop runs, so
    # reverting to a name-keyed directory survives this fixture. The
    # guard says so at the `row` counter rather than implying otherwise
    # here. Review finding on PR #85.
    if printf '%s' "$out" | grep -q 'third_party/b'; then
        ok "two vendored trees of one name are accounted separately"
    else
        bad "a second tree of the same name must not be folded into the first: $out"
    fi
    case "$status" in
        1 | 2) ok "  and the sweep refuses rather than reporting a clean pass" ;;
        *) bad "  expected exit 1 or 2 for an unaccounted tree, got $status" ;;
    esac
else
    skip_or_fail "the twins fixture cannot resolve"
fi

# Its sandbox is `sibling-root`, NOT `outside`: that name belongs to the
# patch-path fixture 380 lines above, and reusing it overwrote that
# sandbox's manifest while leaving its tree and lockfile behind. It still
# reached its assertion, but by accident of ordering. Review finding on
# PR #85.
#
# A LOCAL TREE OUTSIDE THE WORKSPACE ROOT is refused, not silently skipped.
# The selection used to `continue` past it, so unless a patch table happened
# to name it the crate appeared in none of the three sources and the guard
# reported a clean pass with a vulnerable tree compiled in. The help already
# claimed that shape was exit 2. Review finding on PR #85.
mkdir -p "$SANDBOX/sibling-vendor/atty/src"
printf '[package]\nname = "atty"\nversion = "0.2.14"\nedition = "2018"\n' \
    > "$SANDBOX/sibling-vendor/atty/Cargo.toml"
echo '' > "$SANDBOX/sibling-vendor/atty/src/lib.rs"
sibling="$SANDBOX/sibling-root"
mkdir -p "$sibling/apps/probe/src"
printf '[workspace]\nmembers = ["apps/probe"]\nresolver = "2"\n' > "$sibling/Cargo.toml"
{
    printf '[package]\nname = "sibling-probe"\nversion = "0.0.0"\nedition = "2021"\n\n'
    printf '[dependencies]\natty = { path = "../../../sibling-vendor/atty" }\n'
} > "$sibling/apps/probe/Cargo.toml"
echo 'fn main() {}' > "$sibling/apps/probe/src/main.rs"
cp "$ROOT/deny.toml" "$sibling/deny.toml"
if (cd "$sibling" && cargo generate-lockfile >/dev/null 2>&1); then
    out="$(bash "$GUARD" --root "$sibling" 2>&1)"; status=$?
    case "$status" in
        2) ok "a local tree outside the workspace root is refused" ;;
        0) bad "an out-of-root vendored tree was reported as a clean pass — exit 0" ;;
        *) bad "an out-of-root vendored tree — expected exit 2, got $status" ;;
    esac
    if printf '%s' "$out" | grep -q 'outside the workspace root'; then
        ok "  and the refusal names the path it cannot reach"
    else
        bad "  the refusal must name the out-of-root path: $out"
    fi
else
    skip_or_fail "the sibling-root fixture cannot resolve"
fi

# AN UNUSED PATCH POINTING OUTSIDE THE ROOT is the one exit-4 cause with no
# fixture, and it is the branch where `outside` stays empty while `shipped`
# does not -- so exit 4 must fire rather than exit 6. Its plausible
# regression is the silent kind: ADR-0051 says an out-of-root path cannot be
# asked about, so someone "tidying" the patch reader to skip such paths would
# leave `shipped` empty, `unaccounted` empty, and the guard printing
# "nothing is built from a local tree" for a shipped vulnerable tree.
# Review finding on PR #85.
mkdir -p "$SANDBOX/above-root/atty/src"
printf '[package]\nname = "atty"\nversion = "0.2.14"\nedition = "2018"\n' \
    > "$SANDBOX/above-root/atty/Cargo.toml"
echo '' > "$SANDBOX/above-root/atty/src/lib.rs"
outpatch="$SANDBOX/outpatch"
mkdir -p "$outpatch/src"
cat > "$outpatch/Cargo.toml" <<'OUTPATCH'
[package]
name = "outpatch-probe"
version = "0.0.0"
edition = "2021"

[patch.crates-io]
atty = { path = "../above-root/atty" }
OUTPATCH
echo 'fn main() {}' > "$outpatch/src/main.rs"
cp "$ROOT/deny.toml" "$outpatch/deny.toml"
if (cd "$outpatch" && cargo generate-lockfile >/dev/null 2>&1); then
    out="$(bash "$GUARD" --root "$outpatch" 2>&1)"; status=$?
    case "$status" in
        2) ok "an unused patch pointing outside the root is a refusal" ;;
        0) bad "an unused out-of-root patch was reported as a clean pass — exit 0" ;;
        *) bad "an unused out-of-root patch — expected exit 2, got $status" ;;
    esac
    if printf '%s' "$out" | grep -q 'not in the package graph'; then
        ok "  and it is the accounting floor that refuses, not the out-of-root check"
    else
        bad "  expected the shipped-tree reconciliation to fire: $out"
    fi
    # AND THE DIAGNOSTIC, not only the exit code. The clause naming this
    # cause was once dropped from the exit-4 message, and restoring it left
    # nothing pinning it -- the assertion above reads the selection's
    # stderr, not the `die`. Review finding on PR #85.
    if printf '%s' "$out" | grep -q 'points OUTSIDE this workspace root'; then
        ok "  and the refusal explains this particular cause"
    else
        bad "  the exit-4 message must still name the out-of-root patch cause: $out"
    fi
else
    skip_or_fail "the outpatch fixture cannot resolve"
fi

# A PATCH TABLE POINTING INTO A LANDING ZONE, AT A LISTED MEMBER,
# reaches the exit-4 cause that the `die` message names fourth: the
# path lands in the shipped set from the patch reader, the selection
# then skips it because it is in a landing zone AND a workspace
# member, so it is absent from the rows and the accounting floor
# fires. That clause was itself an earlier finding and nothing held
# it.
#
# BOTH HALVES ARE NEEDED and the first attempt had only one: a patch target
# is NOT auto-promoted to a member, so without the explicit `members` entry
# the crate is selected as vendored and probed, exiting 1 on the advisory
# rather than 2 on the floor. Measured, which is the only reason this
# fixture reaches the clause it names.
#
# ASSERTED BY BEHAVIOUR **AND** BY WORDING. The rule this was first written
# under -- grep a clause only where no fixture can reach its cause -- does
# not separate these two clauses: a later reviewer measured that this fixture
# and the out-of-root one are observationally identical, so the cause is held
# by behaviour in both and the wording is held by nothing in either unless it
# is grepped. Both now are.
zonepatch="$SANDBOX/zonepatch"
mkdir -p "$zonepatch/apps/probe/src" "$zonepatch/crates/atty/src"
cat > "$zonepatch/Cargo.toml" <<'ZONEPATCH'
[workspace]
members = ["apps/probe", "crates/atty"]
resolver = "2"

[patch.crates-io]
atty = { path = "crates/atty" }
ZONEPATCH
{
    printf '[package]\nname = "zonepatch-probe"\nversion = "0.0.0"\nedition = "2021"\n\n'
    printf '[dependencies]\natty = "=0.2.14"\n'
} > "$zonepatch/apps/probe/Cargo.toml"
echo 'fn main() {}' > "$zonepatch/apps/probe/src/main.rs"
printf '[package]\nname = "atty"\nversion = "0.2.14"\nedition = "2018"\n' \
    > "$zonepatch/crates/atty/Cargo.toml"
echo '' > "$zonepatch/crates/atty/src/lib.rs"
cp "$ROOT/deny.toml" "$zonepatch/deny.toml"
if (cd "$zonepatch" && cargo generate-lockfile >/dev/null 2>&1); then
    out="$(bash "$GUARD" --root "$zonepatch" 2>&1)"; status=$?
    case "$status" in
        2) ok "a patch path into a landing zone is refused, not skipped" ;;
        0) bad "a patched tree inside a landing zone was reported as clean — exit 0" ;;
        *) bad "a patch path into a landing zone — expected exit 2, got $status" ;;
    esac
    if printf '%s' "$out" | grep -q 'not in the package graph'; then
        ok "  and the accounting floor is what refuses it"
    else
        bad "  expected the shipped-tree reconciliation to fire: $out"
    fi
    # AND THE CLAUSE, because the rule this fixture was written under was
    # wrong. A reviewer measured that this fixture and the out-of-root one
    # produce observationally IDENTICAL output -- same exit code, same
    # selection stderr, same `die` -- so a fixture distinguishes neither from
    # the other, and only the wording does. Deleting this clause left EVERY
    # assertion passing. (It said "all 51" -- correct when measured, stale
    # as soon as assertions were added; the suite is larger now. A count in
    # prose beside the thing it counts goes stale silently, which is the
    # lesson `deny.toml` was corrected for twice in this same series.)
    #
    # Both clauses get a grep; the discriminator offered instead ("grep only
    # where no fixture can reach the cause") does not separate them. Review
    # findings on PR #85.
    if printf '%s' "$out" | grep -q 'landing zone AND is a workspace member'; then
        ok "  and the refusal still names this cause"
    else
        bad "  the exit-4 message must name the first-party-skip cause: $out"
    fi
else
    skip_or_fail "the zonepatch fixture cannot resolve"
fi

# A VENDORED MANIFEST THAT IS A WORKSPACE ROOT RATHER THAN A PACKAGE.
#
# The THIRD clause of the exit-4 message -- counted. An earlier version of
# this comment called it the fifth, which put it AFTER the landing-zone
# clause that the comment twelve lines up correctly calls the fourth, so the
# two contradicted each other.
#
# Deleting the clause left every assertion green, which is why it gets a
# fixture and a grep. THAT RULE IS NOT YET SATISFIED EVERYWHERE, and the
# earlier version of this comment implied it was -- that this clause was the
# last gap. Measured, clause by clause: (1) "behind a disabled feature" has
# neither a fixture nor a grep, and `--all-features` has since narrowed it
# to a patch reached only through a DEPENDENCY's non-default feature, which
# nothing here constructs; (2) "patched but unused" has five fixtures and no
# grep; (3) this one, now both; (4) the landing-zone clause, both; (5) the
# out-of-root clause, both. Three of five.
#
# It is not hypothetical: it is the shape you get from vendoring a
# multi-crate upstream, where `third_party/<up>/Cargo.toml` is a virtual
# `[workspace]` and the real packages sit under it. The disk scan enumerates
# every manifest under a vendored root, a virtual root is never a package in
# the graph, and so it lands at the accounting floor. Same argument that
# earned `norelease` a fixture. Review findings on PR #85.
virtualroot="$SANDBOX/virtualroot"
mkdir -p "$virtualroot/src" "$virtualroot/third_party/upstream/atty/src"
{
    printf '[package]\nname = "virtualroot-probe"\nversion = "0.0.0"\nedition = "2021"\n\n'
    printf '[dependencies]\natty = "=0.2.14"\n\n'
    printf '[patch.crates-io]\natty = { path = "third_party/upstream/atty" }\n'
} > "$virtualroot/Cargo.toml"
echo 'fn main() {}' > "$virtualroot/src/main.rs"
# THE VIRTUAL ROOT: a manifest with a `[workspace]` table and no `[package]`.
printf '[workspace]\nmembers = ["atty"]\nresolver = "2"\n' \
    > "$virtualroot/third_party/upstream/Cargo.toml"
printf '[package]\nname = "atty"\nversion = "0.2.14"\nedition = "2018"\n' \
    > "$virtualroot/third_party/upstream/atty/Cargo.toml"
echo '' > "$virtualroot/third_party/upstream/atty/src/lib.rs"
cp "$ROOT/deny.toml" "$virtualroot/deny.toml"
if (cd "$virtualroot" && cargo generate-lockfile >/dev/null 2>&1); then
    out="$(bash "$GUARD" --root "$virtualroot" 2>&1)"; status=$?
    case "$status" in
        2) ok "a vendored workspace root is refused, not skipped" ;;
        0) bad "a vendored manifest absent from the graph was reported clean — exit 0" ;;
        *) bad "a vendored workspace root — expected exit 2, got $status" ;;
    esac
    if printf '%s' "$out" | grep -q 'not in the package graph'; then
        ok "  and the accounting floor is what refuses it"
    else
        bad "  expected the shipped-tree reconciliation to fire: $out"
    fi
    if printf '%s' "$out" | grep -q 'workspace root rather than a package'; then
        ok "  and the refusal still names this cause"
    else
        bad "  the exit-4 message must name the workspace-root cause: $out"
    fi
else
    skip_or_fail "the virtualroot fixture cannot resolve"
fi

if [ "$failures" -gt 0 ]; then
    printf '\ntest_check_vendored_advisories: %d assertion(s) failed.\n' "$failures" >&2
    exit 1
fi
echo "test_check_vendored_advisories: OK — all assertions passed."
