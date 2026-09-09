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
# The sandbox's vendored directory is a manifest and nothing else: the
# guard reads only a name and a version from it and then asks the
# registry about that version, so no source is needed to exercise it.
#
# Like test_check_dependencies.sh, the advisory cases need `cargo-deny`
# and a reachable database. Without them this degrades to the paths that
# do not: no-patch-block, and an unreadable vendored manifest.

set -uo pipefail

ROOT="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )/../.." && pwd )"
GUARD="$ROOT/tools/checks/check_vendored_advisories.sh"

failures=0
ok()   { printf '  \xe2\x9c\x93 %s\n' "$1"; }
bad()  { printf '  \xe2\x9c\x97 %s\n' "$1"; failures=$((failures + 1)); }

SANDBOX="$(mktemp -d)" || { echo "cannot create a sandbox" >&2; exit 1; }
trap 'rm -rf "$SANDBOX"' EXIT

# A workspace with no [patch.crates-io] block at all.
mkdir -p "$SANDBOX/none/src"
cat > "$SANDBOX/none/Cargo.toml" <<'EOF'
[package]
name = "nothing-vendored"
version = "0.0.0"
edition = "2021"
EOF
cp "$ROOT/deny.toml" "$SANDBOX/none/deny.toml"

bash "$GUARD" --root "$SANDBOX/none" >/dev/null 2>&1
if [ $? -eq 0 ]; then
    ok "a workspace vendoring nothing passes"
else
    bad "a workspace vendoring nothing must pass"
fi

# A patch entry whose vendored manifest is missing.
mkdir -p "$SANDBOX/broken/src"
cat > "$SANDBOX/broken/Cargo.toml" <<'EOF'
[package]
name = "broken"
version = "0.0.0"
edition = "2021"

[patch.crates-io]
atty = { path = "third_party/atty" }
EOF
cp "$ROOT/deny.toml" "$SANDBOX/broken/deny.toml"

bash "$GUARD" --root "$SANDBOX/broken" >/dev/null 2>&1
if [ $? -eq 1 ]; then
    ok "a patch entry with no vendored manifest is an error"
else
    bad "a patch entry with no vendored manifest must exit 1"
fi

if ! cargo deny --version >/dev/null 2>&1; then
    printf '\ntest_check_vendored_advisories: cargo-deny absent — the advisory\n'
    printf 'cases did not run. The guard itself exits 2 in that state, which is\n'
    printf 'what CI relies on; only this self-test degrades.\n'
    [ "$failures" -eq 0 ] || exit 1
    echo "test_check_vendored_advisories: OK — absence paths only."
    exit 0
fi

# THE POSITIVE CASE: a vendored crate that carries advisories.
mkdir -p "$SANDBOX/vulnerable/third_party/atty"
cat > "$SANDBOX/vulnerable/Cargo.toml" <<'EOF'
[package]
name = "vulnerable"
version = "0.0.0"
edition = "2021"

[patch.crates-io]
atty = { path = "third_party/atty" }
EOF
cat > "$SANDBOX/vulnerable/third_party/atty/Cargo.toml" <<'EOF'
[package]
name = "atty"
version = "0.2.14"
edition = "2018"
EOF
cp "$ROOT/deny.toml" "$SANDBOX/vulnerable/deny.toml"

out="$(bash "$GUARD" --root "$SANDBOX/vulnerable" 2>&1)"
status=$?
if [ "$status" -eq 2 ]; then
    printf '\ntest_check_vendored_advisories: the advisory database is unreachable —\n'
    printf 'the positive case did not run.\n'
    [ "$failures" -eq 0 ] || exit 1
    echo "test_check_vendored_advisories: OK — absence paths only."
    exit 0
fi
if [ "$status" -eq 1 ]; then
    ok "a vendored crate carrying an advisory fails"
else
    bad "a vendored atty 0.2.14 must exit 1, got $status"
fi
if printf '%s' "$out" | grep -q 'RUSTSEC-2021-0145'; then
    ok "and the advisory id is reported"
else
    bad "the advisory id must appear in the output"
fi

# The mutation this guard exists to survive: the whole point is that
# cargo-deny alone does NOT see it. Assert that directly, so the day the
# tool starts covering path patches, this fixture says so.
probe="$SANDBOX/deny-alone"
mkdir -p "$probe/third_party/atty/src" "$probe/src"
cp "$SANDBOX/vulnerable/Cargo.toml" "$probe/Cargo.toml"
cp "$ROOT/deny.toml" "$probe/deny.toml"
echo 'fn main() {}' > "$probe/src/main.rs"
printf '[package]\nname = "atty"\nversion = "0.2.14"\nedition = "2018"\n' \
    > "$probe/third_party/atty/Cargo.toml"
echo '' > "$probe/third_party/atty/src/lib.rs"
printf '\n[dependencies]\natty = "=0.2.14"\n' >> "$probe/Cargo.toml"
if (cd "$probe" && cargo generate-lockfile >/dev/null 2>&1); then
    if (cd "$probe" && cargo deny check advisories >/dev/null 2>&1); then
        ok "cargo-deny alone still misses a path-patched crate (the reason this guard exists)"
    else
        bad "cargo-deny now reports path-patched crates — this guard may be redundant, check before deleting it"
    fi
else
    printf '  - could not resolve the deny-alone probe; skipped\n'
fi

if [ "$failures" -gt 0 ]; then
    printf '\ntest_check_vendored_advisories: %d assertion(s) failed.\n' "$failures" >&2
    exit 1
fi
echo "test_check_vendored_advisories: OK — all assertions passed."
