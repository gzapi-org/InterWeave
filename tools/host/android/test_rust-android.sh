#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/host/android/test_rust-android.sh
#
# Self-test for rust-android.sh --check, with rustup and cargo stubbed in
# a scratch CARGO_HOME: every piece present passes; each missing piece
# (rustup at another version, the toolchain, the Android target,
# cargo-ndk, ~/.cargo/bin after /usr/bin on PATH) is named and fails. The
# install path downloads a real toolchain and is not run here.
#
# Exit codes: 0 all assertions passed; 1 otherwise.

set -uo pipefail
HERE="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
UNDER_TEST="$HERE/rust-android.sh"
failures=0
pass() { echo "  ✓ $1"; }
fail() { echo "  ✗ $1" >&2; printf '%s\n' "${2:-}" | sed 's/^/      /' >&2; failures=$((failures + 1)); }
SANDBOX="$(realpath -- "$(mktemp -d)")"; trap 'rm -rf "$SANDBOX"' EXIT
CH="$SANDBOX/cargo"; mkdir -p "$CH/bin" "$SANDBOX/state" "$SANDBOX/sysbin"
printf '[toolchain]\nchannel = "1.98.1"\ncomponents = ["rustfmt", "clippy"]\n' > "$SANDBOX/rust-toolchain.toml"
printf 'RUSTUP_VERSION=1.29.1\nRUSTUP_INIT_SHA256=x\nRUST_ANDROID_TARGET=aarch64-linux-android\nCARGO_NDK_VERSION=4.1.2\n' > "$SANDBOX/pins"
# rustup: what it reports comes from files in state/.
cat > "$CH/bin/rustup" <<EOF
#!/usr/bin/env bash
S="$SANDBOX/state"
case "\$1" in
  --version) echo "rustup \$(cat \$S/rustup 2>/dev/null || echo 1.29.1) (abc 2026-09-01)" ;;
  toolchain) [[ -e \$S/no-toolchain ]] || echo "1.98.1-x86_64-unknown-linux-gnu" ;;
  target) [[ -e \$S/no-target ]] || printf 'aarch64-linux-android\nx86_64-unknown-linux-gnu\n' ;;
  run) shift 2; [[ "\$*" == "cargo ndk --version" && ! -e \$S/no-ndk ]] && echo "cargo-ndk 4.1.2" ;;
esac
EOF
printf '#!/usr/bin/env bash\n' > "$CH/bin/cargo"; cp "$CH/bin/cargo" "$SANDBOX/sysbin/cargo"
chmod +x "$CH/bin/rustup" "$CH/bin/cargo" "$SANDBOX/sysbin/cargo"
run() {  # run [PATH]
    out="$(CARGO_HOME="$CH" ANDROID_TOOLCHAIN_PINS="$SANDBOX/pins" RUST_ANDROID_TOOLCHAIN_FILE="$SANDBOX/rust-toolchain.toml" \
        PATH="${1:-$CH/bin:$SANDBOX/sysbin:/usr/bin:/bin}" bash "$UNDER_TEST" --check 2>&1)"; got=$?
}
expect() { if [[ "$got" -eq "$2" && "$out" == *"$3"* ]]; then pass "$1"; else fail "$1 — wanted exit $2 and '$3', got $got" "$out"; fi; }

echo "rust-android --check"
run; expect "everything present passes" 0 "== all present =="
echo 1.28.2 > "$SANDBOX/state/rustup"; run; expect "rustup at another version" 1 "rustup 1.29.1 is not installed"; rm "$SANDBOX/state/rustup"
touch "$SANDBOX/state/no-toolchain"; run; expect "the pinned toolchain missing" 1 "toolchain 1.98.1 is not installed"; rm "$SANDBOX/state/no-toolchain"
touch "$SANDBOX/state/no-target"; run; expect "the Android target missing" 1 "target aarch64-linux-android is not installed"; rm "$SANDBOX/state/no-target"
touch "$SANDBOX/state/no-ndk"; run; expect "cargo-ndk missing" 1 "cargo-ndk 4.1.2 is not installed"; rm "$SANDBOX/state/no-ndk"
run "$SANDBOX/sysbin:$CH/bin:/usr/bin:/bin"; expect "CARGO_HOME/bin after the system cargo is named" 1 "the system cargo wins"
run "$SANDBOX/sysbin:/usr/bin:/bin"; expect "CARGO_HOME/bin not on PATH is named" 1 "is not on PATH"
printf 'RUSTUP_VERSION=1.29.1\n' > "$SANDBOX/pins"; run; expect "a missing pin is refused" 2 "has no value for"

echo
if [[ $failures -eq 0 ]]; then echo "test_rust-android: OK — all assertions passed."; exit 0; fi
echo "test_rust-android: FAILED — $failures assertion(s)." >&2; exit 1
