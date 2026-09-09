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
build_vendored() {
    local dir="$SANDBOX/$1" crate="$2" ver="$3" form="$4"
    mkdir -p "$dir/src" "$dir/third_party/$crate/src"
    {
        printf '[package]\nname = "%s-probe"\nversion = "0.0.0"\nedition = "2021"\n\n' "$1"
        printf '[dependencies]\n%s = "=%s"\n\n' "$crate" "$ver"
        if [ "$form" = subtable ]; then
            printf '[patch.crates-io.%s]\npath = "third_party/%s"\n' "$crate" "$crate"
        else
            printf '[patch.crates-io]\n%s = { path = "third_party/%s" }\n' "$crate" "$crate"
        fi
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

# THE POSITIVE CASE.
build_vendored vulnerable atty 0.2.14 inline
expect_finding vulnerable "a vendored crate carrying an advisory fails" RUSTSEC-2021-0145

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

if [ "$failures" -gt 0 ]; then
    printf '\ntest_check_vendored_advisories: %d assertion(s) failed.\n' "$failures" >&2
    exit 1
fi
echo "test_check_vendored_advisories: OK — all assertions passed."
