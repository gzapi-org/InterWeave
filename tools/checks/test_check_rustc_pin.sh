#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/test_check_rustc_pin.sh
#
# Self-test for check_rustc_pin.sh.
#
# The check is copied into a sandbox repository whose rust-toolchain.toml
# each case writes, and `rustc` is a stub on PATH printing what each case
# sets — so no case depends on the compiler this host or runner has.
#
# Exit codes:
#   0  all assertions passed
#   1  one or more failed

set -uo pipefail

SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
UNDER_TEST="$SCRIPT_DIR/check_rustc_pin.sh"
[[ -f "$UNDER_TEST" ]] || { echo "test: $UNDER_TEST not found" >&2; exit 1; }

failures=0
SANDBOX="$(mktemp -d)"
trap 'rm -rf "$SANDBOX"' EXIT
REPO="$SANDBOX/repo"
mkdir -p "$REPO/tools/checks" "$SANDBOX/bin"
cp "$UNDER_TEST" "$REPO/tools/checks/"

pass() { echo "  ✓ $1"; }
fail() { echo "  ✗ $1" >&2; printf '%s\n' "${2:-}" | sed 's/^/      /' >&2
         failures=$((failures + 1)); }

# The stub prints $SANDBOX/rustc-says, or fails when $SANDBOX/rustc-fails
# exists; it records the directory it ran in.
cat > "$SANDBOX/bin/rustc" <<EOF
#!/usr/bin/env bash
pwd > "$SANDBOX/rustc-cwd"
[[ -e "$SANDBOX/rustc-fails" ]] && { echo "error: toolchain '1.98.1' is not installed" >&2; exit 1; }
cat "$SANDBOX/rustc-says"
EOF
chmod +x "$SANDBOX/bin/rustc"

pin()   { printf '[toolchain]\n%s\ncomponents = ["rustfmt", "clippy"]\n' "$1" > "$REPO/rust-toolchain.toml"; }
says()  { printf '%s\n' "$1" > "$SANDBOX/rustc-says"; }

# expect <exit> <name> [<output substring>] — run from elsewhere, so the
# check must find the repository root itself.
expect() {
    local want="$1" name="$2" needle="${3:-}" out got
    out="$(cd "$SANDBOX" && PATH="$SANDBOX/bin:$PATH" bash "$REPO/tools/checks/check_rustc_pin.sh" 2>&1)"; got=$?
    if [[ "$got" -ne "$want" ]]; then fail "$name — wanted exit $want, got $got" "$out"; return; fi
    if [[ -n "$needle" && "$out" != *"$needle"* ]]; then fail "$name — output lacks: $needle" "$out"; return; fi
    pass "$name (exit $got)"
}

echo "test_check_rustc_pin"

pin 'channel = "1.98.1"'
says 'rustc 1.98.1 (48a229cea 2026-09-01) (Fedora 1.98.1-1.fc43)'
expect 0 "a distribution's rustc at the pinned version passes" "rustc 1.98.1 is the pinned 1.98.1"
[[ "$(cat "$SANDBOX/rustc-cwd" 2>/dev/null)" == "$REPO" ]] && pass "  and rustc was asked from the repository root, where rustup reads the pin" \
    || fail "rustc ran in $(cat "$SANDBOX/rustc-cwd" 2>/dev/null), not the repository root"
says 'rustc 1.98.1 (48a229cea 2026-09-01)'
expect 0 "rustup's rustc at the pinned version passes"

says 'rustc 1.99.0 (aaaaaaaaa 2026-10-01) (Fedora 1.99.0-1.fc43)'
expect 1 "a newer compiler fails, naming both versions" "the compiler is rustc 1.99.0; rust-toolchain.toml pins 1.98.1"
says 'rustc 1.98.0 (aaaaaaaaa 2026-08-01)'
expect 1 "an older patch release fails" "rustc 1.98.0"
says 'rustc 1.98.10 (aaaaaaaaa 2026-12-01)'
expect 1 "a version the pin is a prefix of fails" "rustc 1.98.10"
says 'rustc 1.98.1-beta.2 (aaaaaaaaa 2026-08-20)'
expect 1 "a pre-release of the pinned version fails" "rustc 1.98.1-beta.2"

pin 'channel = "1.98"'
says 'rustc 1.98.3 (aaaaaaaaa 2026-09-20)'
expect 0 "a minor pin admits any patch release of it"
says 'rustc 1.99.0 (aaaaaaaaa 2026-10-01)'
expect 1 "a minor pin refuses the next minor"
says 'rustc 1.980.0 (aaaaaaaaa 2036-10-01)'
expect 1 "a minor pin is not a string prefix (1.98 against 1.980.0)"

pin 'channel = "1.98.1"'
says 'rustc 1.99.0 (aaaaaaaaa 2026-10-01)'
printf '#!/usr/bin/env bash\necho "rustc 1.98.1 (48a229cea 2026-09-01)"\n' > "$SANDBOX/bin/rustc-pinned"; chmod +x "$SANDBOX/bin/rustc-pinned"
out="$(cd "$SANDBOX" && PATH="$SANDBOX/bin:$PATH" RUSTC="$SANDBOX/bin/rustc-pinned" bash "$REPO/tools/checks/check_rustc_pin.sh" 2>&1)"; got=$?
[[ "$got" -eq 0 ]] && pass "\$RUSTC, the compiler cargo would use, is the one asked (exit 0)" \
    || fail "\$RUSTC should be asked instead of PATH's rustc, got $got" "$out"

touch "$SANDBOX/rustc-fails"
expect 2 "a rustc that cannot run is a failure to check, with its error" "is not installed"
rm -f "$SANDBOX/rustc-fails"
says 'something else entirely'
expect 2 "output with no version is a failure to check" "printed no version"

says 'rustc 1.98.1 (48a229cea 2026-09-01)'
pin 'channel = "stable"'
expect 2 "a floating channel names no version: a failure to check" "names no version"
pin 'channel = "nightly-2026-09-01"'
expect 2 "a nightly channel names no version either" "names no version"
pin 'profile = "minimal"'
expect 2 "no channel is a failure to check" "has no channel"
rm -f "$REPO/rust-toolchain.toml"
expect 2 "no rust-toolchain.toml is a failure to check" "no rust-toolchain.toml"

help_out="$(bash "$UNDER_TEST" --help 2>/dev/null)"
if [[ "$help_out" == *"rust-toolchain.toml pins"* ]]; then pass "--help prints the help block"; else fail "--help should print the help block" "$help_out"; fi

echo
if (( failures > 0 )); then
    echo "test_check_rustc_pin: $failures failure(s)" >&2
    exit 1
fi
echo "test_check_rustc_pin: OK — all assertions passed."
