#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/host/android/rust-android.sh
#
# The per-account half of the Android toolchain: rustup, the toolchain
# rust-toolchain.toml pins with the Android target added, and cargo-ndk,
# all in the running account's home (~/.rustup, ~/.cargo) at the versions
# android-toolchain.pins names.
#
#   bash tools/host/android/rust-android.sh            install (idempotent)
#   bash tools/host/android/rust-android.sh --check    read-only
#
# WHY NOT THE SYSTEM RUST. The host's rustc is the distribution's package
# (Fedora 1.98.1), which ships no standard library for Android, and has no
# rustup to add one. rustup installs the same pinned channel with the
# target beside it. Its proxies in ~/.cargo/bin must come before /usr/bin
# on PATH for cargo to use them; this script says so and does not edit
# your shell files.
#
# Exit codes: 0 done / all present; 1 something missing (--check, each
# named); 2 usage, environment or a failed step.

set -uo pipefail

HERE="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
REPO="$( cd -- "$HERE/../../.." && pwd )"
PINS_FILE="${ANDROID_TOOLCHAIN_PINS:-$HERE/android-toolchain.pins}"
TOOLCHAIN_FILE="${RUST_ANDROID_TOOLCHAIN_FILE:-$REPO/rust-toolchain.toml}"
CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}"
export CARGO_HOME RUSTUP_HOME
me="rust-android"

die() { echo "$me: $*" >&2; exit 2; }
PROBLEMS=0
ok()  { printf '  OK    %s\n' "$*"; }
bad() { printf '  FAIL  %s\n' "$*"; PROBLEMS=$((PROBLEMS + 1)); }

MODE=install
case "${1:-}" in
    "") ;;
    --check) MODE=check ;;
    -h|--help) awk 'NR > 3 && !/^#/ { exit } NR > 3 { sub(/^# ?/, ""); print }' "$0"; exit 0 ;;
    *) die "unknown argument '$1' (--help)" ;;
esac

pin() { sed -n "s/^$1=//p" "$PINS_FILE" | head -1; }
RUSTUP_VERSION="$(pin RUSTUP_VERSION)" RUSTUP_SHA="$(pin RUSTUP_INIT_SHA256)"
TARGET="$(pin RUST_ANDROID_TARGET)" NDK_TOOL="$(pin CARGO_NDK_VERSION)"
for v in RUSTUP_VERSION RUSTUP_SHA TARGET NDK_TOOL; do [[ -n "${!v}" ]] || die "$PINS_FILE has no value for $v"; done
CHANNEL="$(sed -n 's/^channel[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$TOOLCHAIN_FILE" 2>/dev/null | head -1)"
[[ -n "$CHANNEL" ]] || die "no channel in $TOOLCHAIN_FILE"
COMPONENTS="$(sed -n 's/^components[[:space:]]*=[[:space:]]*\[\(.*\)\].*/\1/p' "$TOOLCHAIN_FILE" | tr -d ' "' )"

RUSTUP="$CARGO_HOME/bin/rustup"
check_all() {
    if [[ -x "$RUSTUP" ]] && "$RUSTUP" --version 2>/dev/null | grep -q "^rustup $RUSTUP_VERSION "; then ok "rustup $RUSTUP_VERSION"
    else bad "rustup $RUSTUP_VERSION is not installed in $CARGO_HOME"; return; fi
    if "$RUSTUP" toolchain list 2>/dev/null | grep -q "^$CHANNEL-"; then ok "toolchain $CHANNEL (rust-toolchain.toml)"
    else bad "toolchain $CHANNEL is not installed"; fi
    if "$RUSTUP" target list --installed --toolchain "$CHANNEL" 2>/dev/null | grep -qx "$TARGET"; then ok "target $TARGET"
    else bad "target $TARGET is not installed for $CHANNEL"; fi
    # cargo-ndk answers only as a cargo subcommand (`cargo ndk`); called
    # directly it prints a refusal and exits 0.
    if PATH="$CARGO_HOME/bin:$PATH" "$RUSTUP" run "$CHANNEL" cargo ndk --version 2>/dev/null | grep -qx "cargo-ndk $NDK_TOOL"; then ok "cargo-ndk $NDK_TOOL"
    else bad "cargo-ndk $NDK_TOOL is not installed"; fi
    case ":$PATH:" in
        *":$CARGO_HOME/bin:"*) [[ "$(command -v cargo)" == "$CARGO_HOME/bin/cargo" ]] && ok "$CARGO_HOME/bin comes first on PATH" \
                                   || bad "$CARGO_HOME/bin is on PATH but after $(dirname "$(command -v cargo)"): the system cargo wins" ;;
        *) bad "$CARGO_HOME/bin is not on PATH: add it before /usr/bin (for example in ~/.bashrc)" ;;
    esac
}

if [[ "$MODE" == check ]]; then
    echo "== $me --check =="
    check_all
    if [[ "$PROBLEMS" -eq 0 ]]; then echo "== all present =="; exit 0; fi
    echo "== $PROBLEMS problem(s) =="; exit 1
fi

echo "== $me: rustup $RUSTUP_VERSION, $CHANNEL + $TARGET, cargo-ndk $NDK_TOOL =="
command -v curl >/dev/null && command -v sha256sum >/dev/null || die "curl and sha256sum are required"
if ! { [[ -x "$RUSTUP" ]] && "$RUSTUP" --version 2>/dev/null | grep -q "^rustup $RUSTUP_VERSION "; }; then
    tmp="$(mktemp -d)" || die "mktemp failed"; trap 'rm -rf "$tmp"' EXIT
    url="https://static.rust-lang.org/rustup/archive/$RUSTUP_VERSION/x86_64-unknown-linux-gnu/rustup-init"
    curl -fL --retry 3 --silent --show-error -o "$tmp/rustup-init" "$url" || die "download failed: $url"
    [[ "$(sha256sum "$tmp/rustup-init" | cut -d' ' -f1)" == "$RUSTUP_SHA" ]] || die "rustup-init does not match its pinned sha256"
    chmod +x "$tmp/rustup-init"
    "$tmp/rustup-init" -y --no-modify-path --default-toolchain none --profile minimal >/dev/null || die "rustup-init failed"
fi
args=(toolchain install "$CHANNEL" --profile minimal --target "$TARGET")
[[ -n "$COMPONENTS" ]] && args+=(--component "$COMPONENTS")
"$RUSTUP" "${args[@]}" || die "rustup could not install $CHANNEL with $TARGET"
"$RUSTUP" run "$CHANNEL" cargo install cargo-ndk --version "$NDK_TOOL" --locked --root "$CARGO_HOME" \
    || die "cargo install cargo-ndk $NDK_TOOL failed"
echo
check_all
[[ "$PROBLEMS" -eq 0 ]] && { echo "== done =="; exit 0; }
echo "== installed, $PROBLEMS problem(s) left (above) =="; exit 1
