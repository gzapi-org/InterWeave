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
mkdir -p "$S/repo"
fake_ndk() {  # <dir> <revision> [no-clang]
    mkdir -p "$1/toolchains/llvm/prebuilt/linux-x86_64/bin"
    printf 'Pkg.Desc = Android NDK\nPkg.Revision = %s\n' "$2" > "$1/source.properties"
    for b in aarch64-linux-android30-clang llvm-ar; do
        [[ "$b" != *clang || -z "${3:-}" ]] || continue
        printf '#!/bin/sh\n' > "$1/toolchains/llvm/prebuilt/linux-x86_64/bin/$b"
        chmod +x "$1/toolchains/llvm/prebuilt/linux-x86_64/bin/$b"
    done
}
fake_ndk "$S/ndk" 28.2.13676358                   # the pin, as ANDROID_NDK_HOME
fake_ndk "$S/sdk/ndk/28.2.13676358" 28.2.13676358 # the pin, inside an SDK
fake_ndk "$S/image-ndk" 27.3.13750724             # a runner image's default
fake_ndk "$S/noclang" 28.2.13676358 no-clang
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
           echo "CC $CC_aarch64_linux_android"; echo "AR $AR_aarch64_linux_android";
           echo "ANDROID_JAR ${ANDROID_JAR:-}"; } > "$FAKE_LOG"
         [ -z "${FAKE_CHECK_FAIL:-}" ] ;;
esac
C
chmod +x "$S/cargo"
run() { env -u ANDROID_HOME -u ANDROID_SDK_ROOT -u ANDROID_NDK_HOME -u JAVA_HOME -u ANDROID_JAR ANDROID_CHECK_REPO="$S/repo" \
            ANDROID_TOOLCHAIN_PINS="${PINS:-$S/pins}" CARGO="$S/cargo" FAKE_LOG="$S/log" \
            ${NDK-ANDROID_NDK_HOME=$S/ndk} ${SDK:+ANDROID_HOME=$SDK} "$@" bash "$SUT" ${ARGS:-} 2>&1; }

echo "check_android_target: the pins decide target, NDK and API; cargo gets the NDK's tools"
rm -f "$S/log"; out="$(FAKE_PKGS="interweave-profile-config other" run)"; rc=$?
[[ $rc -eq 0 ]] && ok "profile-config alone: exit 0" || bad "rc $rc" "$out"
grep -q '^ARGS check --locked --target aarch64-linux-android -p interweave-profile-config$' "$S/log" \
    && ok "checks profile-config for aarch64-linux-android, --locked" || bad "cargo argv" "$(cat "$S/log" 2>/dev/null)"
grep -q "^LINKER $S/ndk/.*/aarch64-linux-android30-clang$" "$S/log" && ok "the linker is the NDK clang at the LOWEST platform (30, not 36)" || bad "linker" "$(cat "$S/log")"
grep -q "^CC $S/ndk/.*/aarch64-linux-android30-clang$" "$S/log" && ok "CC for the target is the same clang" || bad "CC" "$(cat "$S/log")"
grep -q "^AR $S/ndk/.*/llvm-ar$" "$S/log" && ok "AR for the target is the NDK's llvm-ar" || bad "AR" "$(cat "$S/log")"
grep -q "interweave-transport-embedded is not in the workspace yet" <<<"$out" && ok "says the embedded crate is not checked yet" || bad "silent about the absent crate" "$out"
grep -qE "NDK 28.2.13676358 at [^,]+, API 30" <<<"$out" && ok "names the NDK and API it used" || bad "versions not named" "$out"

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
out="$(FAKE_PKGS="interweave-profile-config" NDK="ANDROID_NDK_HOME=$S/noclang" run)"; rc=$?
[[ $rc -eq 2 && "$out" == *"without API 30's clang"* ]] && ok "the pinned NDK without the API's clang" || bad "rc $rc" "$out"

echo "check_android_target: only the PINNED NDK is used, whatever the environment points at"
rm -f "$S/log"; out="$(FAKE_PKGS="interweave-profile-config" NDK="ANDROID_NDK_HOME=$S/image-ndk" SDK="$S/sdk" run)"; rc=$?
[[ $rc -eq 0 ]] && grep -q "^LINKER $S/sdk/ndk/28.2.13676358/" "$S/log" \
    && ok "the SDK's ndk/<pin> wins over an ANDROID_NDK_HOME at the image's NDK" || bad "rc $rc" "$(cat "$S/log" 2>/dev/null; echo "$out")"
out="$(FAKE_PKGS="interweave-profile-config" NDK="ANDROID_NDK_HOME=$S/image-ndk" run)"; rc=$?
[[ $rc -eq 2 && "$out" == *"no NDK 28.2.13676358"*"is '27.3.13750724'"* ]] \
    && ok "an ANDROID_NDK_HOME of another revision alone: exit 2, both revisions named" || bad "rc $rc" "$out"
rm -f "$S/log"; out="$(FAKE_PKGS="interweave-profile-config" NDK="" SDK="$S/sdk" run)"; rc=$?
[[ $rc -eq 0 ]] && grep -q "^LINKER $S/sdk/ndk/28.2.13676358/" "$S/log" \
    && ok "ANDROID_HOME alone finds ndk/<pin>" || bad "rc $rc" "$out"
grep -v '^PKG_6=' "$S/pins" > "$S/pins-nondk"
out="$(FAKE_PKGS="interweave-profile-config" PINS="$S/pins-nondk" run)"; rc=$?
[[ $rc -eq 2 && "$out" == *"no ndk;<version> package"* ]] && ok "pins without an NDK" || bad "rc $rc" "$out"
grep -v '^PKG_[23]=' "$S/pins" > "$S/pins-noapi"
out="$(FAKE_PKGS="interweave-profile-config" PINS="$S/pins-noapi" run)"; rc=$?
[[ $rc -eq 2 && "$out" == *"no platforms;android-<n> package"* ]] && ok "pins without a platform" || bad "rc $rc" "$out"
sed 's/^RUST_ANDROID_TARGET=.*/RUST_ANDROID_TARGET=x86_64-unknown-linux-gnu/' "$S/pins" > "$S/pins-badtarget"
out="$(FAKE_PKGS="interweave-profile-config" PINS="$S/pins-badtarget" run)"; rc=$?
[[ $rc -eq 2 && "$out" == *"not a <arch>-linux-android triple"* ]] && ok "a target that is not Android" || bad "rc $rc" "$out"

echo "check_android_target: the human-android crates, with their features and the JDK and jar Slint needs"
mkdir -p "$S/jdk/bin" "$S/sdk/platforms/android-30" "$S/sdk/platforms/android-36"
printf '#!/bin/sh\n' > "$S/jdk/bin/javac"; chmod +x "$S/jdk/bin/javac"
: > "$S/sdk/platforms/android-30/android.jar"; : > "$S/sdk/platforms/android-36/android.jar"
ALL="interweave-profile-config interweave-transport-embedded interweave-human-android-platform interweave-human-android"
rm -f "$S/log"; out="$(FAKE_PKGS="$ALL" NDK="" SDK="$S/sdk" run JAVA_HOME="$S/jdk")"; rc=$?
[[ $rc -eq 0 ]] && ok "all four packages: exit 0" || bad "rc $rc" "$out"
grep -q -- '-p interweave-human-android-platform -p interweave-human-android --features interweave-human-android-platform/dev-stand-ins$' "$S/log" \
    && ok "one cargo check, the platform crate with dev-stand-ins" || bad "cargo argv" "$(cat "$S/log" 2>/dev/null)"
grep -qx "ANDROID_JAR $S/sdk/platforms/android-36/android.jar" "$S/log" \
    && ok "ANDROID_JAR is the HIGHEST pinned platform's (36, not 30)" || bad "ANDROID_JAR" "$(cat "$S/log" 2>/dev/null)"
grep -q "JDK at $S/jdk, ANDROID_JAR at" <<<"$out" && ok "the log names the JDK and the jar" || bad "not named" "$out"
rm -f "$S/log"; out="$(FAKE_PKGS="interweave-profile-config interweave-human-android-platform" run)"; rc=$?
[[ $rc -eq 0 ]] && grep -q -- '-p interweave-human-android-platform --features interweave-human-android-platform/dev-stand-ins$' "$S/log" \
    && grep -qx "ANDROID_JAR " "$S/log" \
    && ok "the platform crate alone needs no JDK, and gets no ANDROID_JAR" || bad "rc $rc" "$(cat "$S/log" 2>/dev/null; echo "$out")"
rm -f "$S/log"; out="$(FAKE_PKGS="interweave-profile-config interweave-transport-embedded" run)"; rc=$?
# The run must reach cargo, or an absent log would pass the grep below.
if [[ $rc -ne 0 || ! -f "$S/log" ]]; then bad "no cargo check ran (rc $rc)" "$out"
elif grep -q -- '--features' "$S/log"; then bad "features passed with no featured package" "$(cat "$S/log")"
else ok "no --features when no listed package is checked"; fi
out="$(FAKE_PKGS="$ALL" NDK="" SDK="$S/sdk" run)"; rc=$?
[[ $rc -eq 2 && "$out" == *"interweave-human-android needs a JDK"* ]] && ok "human-android without JAVA_HOME: exit 2" || bad "rc $rc" "$out"
out="$(FAKE_PKGS="$ALL" NDK="" SDK="$S/sdk" run JAVA_HOME="$S/nowhere")"; rc=$?
[[ $rc -eq 2 && "$out" == *"with no bin/javac"* ]] && ok "a JAVA_HOME without javac: exit 2" || bad "rc $rc" "$out"
out="$(FAKE_PKGS="$ALL" run JAVA_HOME="$S/jdk")"; rc=$?
[[ $rc -eq 2 && "$out" == *"needs the SDK: ANDROID_HOME is not set"* ]] && ok "an NDK alone, no SDK: exit 2" || bad "rc $rc" "$out"
rm "$S/sdk/platforms/android-36/android.jar"
out="$(FAKE_PKGS="$ALL" NDK="" SDK="$S/sdk" run JAVA_HOME="$S/jdk")"; rc=$?
[[ $rc -eq 2 && "$out" == *"needs $S/sdk/platforms/android-36/android.jar"* ]] && ok "the compileSdk platform missing: exit 2, the jar named" || bad "rc $rc" "$out"

echo "check_android_target: a failing cargo check is exit 1"
out="$(FAKE_PKGS="interweave-profile-config" run FAKE_CHECK_FAIL=1)"; rc=$?
[[ $rc -eq 1 && "$out" == *"cargo check failed"* ]] && ok "exit 1, said" || bad "rc $rc" "$out"

echo
if (( FAIL == 0 )); then echo "test_check_android_target: OK — $PASS assertion(s)."
else echo "test_check_android_target: FAILED — $FAIL of $((PASS + FAIL))."; exit 1; fi
