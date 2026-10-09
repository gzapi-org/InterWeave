#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/ci/test_check_android_target.sh
#
# Self-test for check_android_target.sh, with cargo and the NDK faked: the
# target, NDK and API level come from the pins (the LOWEST platform), the
# linker, CC and AR reach cargo for that target, the default packages are
# checked only when the workspace has them, and every missing input is a
# loud exit 2 — a cargo check failure exit 1.
set -uo pipefail
HERE="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
SUT="$HERE/check_android_target.sh"
PASS=0; FAIL=0
ok()  { PASS=$((PASS+1)); printf '  ok   %s\n' "$1"; }
bad() { FAIL=$((FAIL+1)); printf '  FAIL %s\n' "$1"; [[ -n "${2:-}" ]] && printf '%s\n' "$2" | sed 's/^/       /' | head -6; }

S="$(mktemp -d)"; trap 'rm -rf "$S"' EXIT
mkdir -p "$S/repo" "$S/ndk/toolchains/llvm/prebuilt/linux-x86_64/bin"
for b in aarch64-linux-android30-clang llvm-ar; do
    printf '#!/bin/sh\n' > "$S/ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/$b"
    chmod +x "$S/ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/$b"
done
cat > "$S/pins" <<'P'
PKG_2=platforms;android-36@2
PKG_3=platforms;android-30@3
PKG_6=ndk;28.2.13676358@28.2.13676358
RUST_ANDROID_TARGET=aarch64-linux-android
P
# The fake cargo: metadata lists $FAKE_PKGS; check records its argv and
# the three variables, and fails when FAKE_CHECK_FAIL is set.
cat > "$S/cargo" <<'C'
#!/usr/bin/env bash
case "$1" in
  metadata) printf '{"packages":['; sep=""; for p in $FAKE_PKGS; do printf '%s{"name":"%s"}' "$sep" "$p"; sep=","; done; printf ']}\n' ;;
  check) { echo "ARGS $*"; echo "LINKER $CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER";
           echo "CC $CC_aarch64_linux_android"; echo "AR $AR_aarch64_linux_android"; } > "$FAKE_LOG"
         [ -z "${FAKE_CHECK_FAIL:-}" ] ;;
esac
C
chmod +x "$S/cargo"
run() { env -u ANDROID_HOME -u ANDROID_SDK_ROOT ANDROID_CHECK_REPO="$S/repo" ANDROID_TOOLCHAIN_PINS="${PINS:-$S/pins}" CARGO="$S/cargo" \
            ANDROID_NDK_HOME="${NDK-$S/ndk}" FAKE_LOG="$S/log" "$@" bash "$SUT" ${ARGS:-} 2>&1; }

echo "check_android_target: the pins decide target, NDK and API; cargo gets the NDK's tools"
rm -f "$S/log"; out="$(FAKE_PKGS="interweave-profile-config other" run)"; rc=$?
[[ $rc -eq 0 ]] && ok "profile-config alone: exit 0" || bad "rc $rc" "$out"
grep -q '^ARGS check --locked --target aarch64-linux-android -p interweave-profile-config$' "$S/log" \
    && ok "checks profile-config for aarch64-linux-android, --locked" || bad "cargo argv" "$(cat "$S/log" 2>/dev/null)"
grep -q "^LINKER $S/ndk/.*/aarch64-linux-android30-clang$" "$S/log" && ok "the linker is the NDK clang at the LOWEST platform (30, not 36)" || bad "linker" "$(cat "$S/log")"
grep -q "^CC $S/ndk/.*/aarch64-linux-android30-clang$" "$S/log" && ok "CC for the target is the same clang" || bad "CC" "$(cat "$S/log")"
grep -q "^AR $S/ndk/.*/llvm-ar$" "$S/log" && ok "AR for the target is the NDK's llvm-ar" || bad "AR" "$(cat "$S/log")"
grep -q "interweave-transport-embedded is not in the workspace yet" <<<"$out" && ok "says the embedded crate is not checked yet" || bad "silent about the absent crate" "$out"
grep -q "NDK 28.2.13676358, API 30" <<<"$out" && ok "names the NDK and API it used" || bad "versions not named" "$out"

echo "check_android_target: the embedded crate joins once the workspace has it"
rm -f "$S/log"; out="$(FAKE_PKGS="interweave-profile-config interweave-transport-embedded" run)"; rc=$?
[[ $rc -eq 0 ]] && grep -q -- '-p interweave-profile-config -p interweave-transport-embedded$' "$S/log" \
    && ok "both packages, one cargo check" || bad "embedded not added (rc $rc)" "$(cat "$S/log" 2>/dev/null; echo "$out")"

echo "check_android_target: a named package must exist"
out="$(FAKE_PKGS="interweave-profile-config" ARGS="nope" run)"; rc=$?
[[ $rc -eq 2 && "$out" == *"no package 'nope'"* ]] && ok "unknown named package: exit 2, named" || bad "rc $rc" "$out"

echo "check_android_target: every missing input is a loud exit 2"
out="$(FAKE_PKGS="other" run)"; rc=$?
[[ $rc -eq 2 && "$out" == *"none of"* ]] && ok "no target crate in the workspace" || bad "rc $rc" "$out"
out="$(FAKE_PKGS="interweave-profile-config" NDK="" run)"; rc=$?
[[ $rc -eq 2 && "$out" == *"neither ANDROID_NDK_HOME nor ANDROID_HOME"* ]] && ok "no NDK location" || bad "rc $rc" "$out"
out="$(FAKE_PKGS="interweave-profile-config" NDK="$S/repo" run)"; rc=$?
[[ $rc -eq 2 && "$out" == *"is NDK 28.2.13676358 installed"* ]] && ok "an NDK without the clang" || bad "rc $rc" "$out"
grep -v '^PKG_6=' "$S/pins" > "$S/pins-nondk"
out="$(FAKE_PKGS="interweave-profile-config" PINS="$S/pins-nondk" run)"; rc=$?
[[ $rc -eq 2 && "$out" == *"no ndk;<version> package"* ]] && ok "pins without an NDK" || bad "rc $rc" "$out"
grep -v '^PKG_[23]=' "$S/pins" > "$S/pins-noapi"
out="$(FAKE_PKGS="interweave-profile-config" PINS="$S/pins-noapi" run)"; rc=$?
[[ $rc -eq 2 && "$out" == *"no platforms;android-<n> package"* ]] && ok "pins without a platform" || bad "rc $rc" "$out"
sed 's/^RUST_ANDROID_TARGET=.*/RUST_ANDROID_TARGET=x86_64-unknown-linux-gnu/' "$S/pins" > "$S/pins-badtarget"
out="$(FAKE_PKGS="interweave-profile-config" PINS="$S/pins-badtarget" run)"; rc=$?
[[ $rc -eq 2 && "$out" == *"not a <arch>-linux-android triple"* ]] && ok "a target that is not Android" || bad "rc $rc" "$out"

echo "check_android_target: a failing cargo check is exit 1"
out="$(FAKE_PKGS="interweave-profile-config" run FAKE_CHECK_FAIL=1)"; rc=$?
[[ $rc -eq 1 && "$out" == *"cargo check failed"* ]] && ok "exit 1, said" || bad "rc $rc" "$out"

echo
if (( FAIL == 0 )); then echo "test_check_android_target: OK — $PASS assertion(s)."
else echo "test_check_android_target: FAILED — $FAIL of $((PASS + FAIL))."; exit 1; fi
