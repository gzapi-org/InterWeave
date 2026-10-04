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

# The compiler is chosen by these too, and a caller's value would replace
# the stub every case below relies on; the cases that need one set it.
unset RUSTC CARGO_BUILD_RUSTC
export CARGO_HOME="/nonexistent-cargo-home"

failures=0
SANDBOX="$(mktemp -d)"
trap 'rm -rf "$SANDBOX"' EXIT
# The check walks from the repository up to / for cargo configs, so one
# above the sandbox that sets a rustc key would decide every case; refuse,
# by name, instead. Only such a one: any other config changes no case.
# The pattern is the check's own line.
eval "$(grep -m1 '^rustc_key_re=' "$UNDER_TEST")"
[[ -n "${rustc_key_re:-}" ]] || { echo "test_check_rustc_pin: no rustc_key_re= line in $UNDER_TEST" >&2; exit 1; }
d="$SANDBOX"
while :; do
    for f in "$d/.cargo/config.toml" "$d/.cargo/config"; do
        if [[ -f "$f" ]] && grep -Eq "$rustc_key_re" "$f"; then
            echo "test_check_rustc_pin: cannot run under $f (above the sandbox, and it sets a rustc key); set TMPDIR elsewhere" >&2; exit 1
        fi
    done
    [[ "$d" == / ]] && break
    d="$(dirname "$d")"
done
REPO="$SANDBOX/repo"
mkdir -p "$REPO/tools/checks" "$SANDBOX/bin"
cp "$UNDER_TEST" "$REPO/tools/checks/"
# A git work tree, as the repository is: the check lists its tracked files.
git -C "$REPO" init -q

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
says 'rustc 1.98.1-beta.2 (aaaaaaaaa 2026-08-20)'
expect 1 "a minor pin refuses a pre-release of it" "rustc 1.98.1-beta.2"
says 'rustc 1x98.3 (aaaaaaaaa 2026-09-20)'
expect 1 "a minor pin's dots are literal (1x98.3 is not 1.98)"
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
out="$(cd "$SANDBOX" && PATH="$SANDBOX/bin:$PATH" CARGO_BUILD_RUSTC="$SANDBOX/bin/rustc-pinned" bash "$REPO/tools/checks/check_rustc_pin.sh" 2>&1)"; got=$?
[[ "$got" -eq 0 ]] && pass "\$CARGO_BUILD_RUSTC is asked when \$RUSTC is unset (exit 0)" \
    || fail "\$CARGO_BUILD_RUSTC should be asked instead of PATH's rustc, got $got" "$out"
out="$(cd "$SANDBOX" && PATH="$SANDBOX/bin:$PATH" RUSTC="$SANDBOX/bin/rustc" CARGO_BUILD_RUSTC="$SANDBOX/bin/rustc-pinned" bash "$REPO/tools/checks/check_rustc_pin.sh" 2>&1)"; got=$?
[[ "$got" -eq 1 ]] && pass "  and \$RUSTC wins over it, as in cargo (exit 1)" \
    || fail "\$RUSTC should win over \$CARGO_BUILD_RUSTC, got $got" "$out"

# A key spelled rustc in a cargo config in scope is a failure to check, in
# every TOML spelling, in the repository, a parent directory or under
# $CARGO_HOME; rustc-wrapper and other [build] keys are not.
says 'rustc 1.98.1 (48a229cea 2026-09-01)'
mkdir -p "$REPO/.cargo"
printf '[build]\nrustc = "/opt/rust-1.99/bin/rustc"\n' > "$REPO/.cargo/config.toml"
expect 2 "build.rustc under [build] in the repository's config is named" ".cargo/config.toml sets a key spelled rustc"
printf 'build.rustc = "/opt/rust-1.99/bin/rustc"\n' > "$REPO/.cargo/config.toml"
expect 2 "a dotted build.rustc is named too" "sets a key spelled rustc"
for form in 'build = { rustc = "/opt/r/rustc" }' '[build]\n"rustc" = "/opt/r/rustc"' '[ build ]\nrustc="/opt/r/rustc"' 'build . rustc = "/opt/r/rustc"' "build.'rustc' = '/opt/r/rustc'"; do
    printf "$form\n" > "$REPO/.cargo/config.toml"
    expect 2 "the spelling $(printf "$form" | tr '\n' ' ' | sed 's/ $//') is named" "sets a key spelled rustc"
done
printf '[build]\nrustc-wrapper = "sccache"\nrustc-workspace-wrapper = "x"\nrustflags = ["-Dwarnings"]\n' > "$REPO/.cargo/config.toml"
expect 0 "rustc-wrapper and other [build] keys are not a compiler choice"
rm -f "$REPO/.cargo/config.toml"
mkdir -p "$SANDBOX/.cargo"
printf '[build]\nrustc = "/opt/rust-1.99/bin/rustc"\n' > "$SANDBOX/.cargo/config.toml"
expect 2 "a parent directory's config is searched too" "$SANDBOX/.cargo/config.toml sets"
rm -rf "$SANDBOX/.cargo"

# A tracked cargo config below the root is read by cargo run from there,
# and not by this check: a failure to check. The root's own is not.
mkdir -p "$REPO/.cargo" "$REPO/crates/x/.cargo"
printf '[alias]\nxtask = "run -p xtask --"\n' > "$REPO/.cargo/config.toml"
git -C "$REPO" add .cargo/config.toml
expect 0 "the root's own tracked cargo config is in scope, not below it"
printf '[net]\nretry = 2\n' > "$REPO/crates/x/.cargo/config.toml"
git -C "$REPO" add crates/x/.cargo/config.toml
expect 2 "a tracked cargo config below the root is named" "crates/x/.cargo/config.toml is a cargo config below the repository root"
git -C "$REPO" rm -q --cached -r .cargo crates
rm -rf "$REPO/.cargo" "$REPO/crates"

# Not a git work tree: the tracked configs cannot be listed, so the check
# cannot say none is below the root -- a failure to check, not a pass.
printf '#!/usr/bin/env bash\necho "fatal: not a git repository (or any of the parent directories): .git" >&2\nexit 128\n' > "$SANDBOX/bin/git"
chmod +x "$SANDBOX/bin/git"
expect 2 "a tree git cannot list is a failure to check" "cannot list the tree's tracked files"
rm -f "$SANDBOX/bin/git"

# A config in scope that cannot be read cannot be said to set no rustc
# key. Root reads it regardless, so the case does not apply there.
mkdir -p "$REPO/.cargo"
printf '[net]\nretry = 2\n' > "$REPO/.cargo/config.toml"
chmod 000 "$REPO/.cargo/config.toml"
if [[ "$(id -u)" -eq 0 ]]; then
    pass "(running as root, which reads a mode-000 file: the unreadable-config case does not apply)"
else
    expect 2 "a config in scope that cannot be read is a failure to check" "cannot read $REPO/.cargo/config.toml"
fi
chmod 600 "$REPO/.cargo/config.toml"
rm -rf "$REPO/.cargo"
mkdir -p "$SANDBOX/cargo-home"
printf '[build]\nrustc = "/opt/rust-1.99/bin/rustc"\n' > "$SANDBOX/cargo-home/config.toml"
out="$(cd "$SANDBOX" && PATH="$SANDBOX/bin:$PATH" CARGO_HOME="$SANDBOX/cargo-home" bash "$REPO/tools/checks/check_rustc_pin.sh" 2>&1)"; got=$?
if [[ "$got" -eq 2 && "$out" == *"cargo-home/config.toml sets a key spelled rustc"* ]]; then pass "build.rustc in \$CARGO_HOME's config is named (exit 2)"
else fail "build.rustc under \$CARGO_HOME should be exit 2, got $got" "$out"; fi

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
