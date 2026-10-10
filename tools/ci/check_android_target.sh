#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/ci/check_android_target.sh
#
# `cargo check --target aarch64-linux-android` over the crates Stage 17's
# step 1 builds for the phone, with the NDK and API level the host toolchain
# pins — so "built for the aarch64 target" is checked somewhere (no account
# on develop-qzapp has an Android std; the CI runner gets one from rustup).
#
#   bash tools/ci/check_android_target.sh [<package>...]
#
# The packages default to ANDROID_CHECK_PACKAGES below, each checked only
# when the workspace has it, so a crate listed ahead of its landing joins
# on its own the day it lands. A package NAMED on the command line that the
# workspace lacks is an error.
#
# Every version comes from tools/host/android/android-toolchain.pins, the
# one place the host reads them: the target from RUST_ANDROID_TARGET, the
# NDK from its `ndk;<version>` package, the API level from the LOWEST
# `platforms;android-<n>` package (the oldest device the app supports, so
# a symbol newer than it fails here, not on the phone). The linker and C
# compiler are that NDK's clang for that level: `cargo check` links
# nothing, but a build script compiling C for the target (the cc crate)
# reads CC_<target>, and an unset one falls back to the host's cc, which
# cannot target Android.
#
# Environment: ANDROID_HOME with the NDK under ndk/<version> (the runner's
# SDK, after sdkmanager installs the pin), or ANDROID_NDK_HOME at an NDK of
# exactly the pinned revision (its source.properties says which).
#
# interweave-human-android needs more: Slint's Android backend build script
# compiles a Java helper with the JDK's javac against an android.jar and
# dexes it with the SDK's d8. So for it JAVA_HOME must hold bin/javac and
# ANDROID_HOME the SDK, and ANDROID_JAR is set to the HIGHEST pinned
# platform's jar (the compileSdk; the helper needs API 33's classes, and
# left to itself the build script takes the lowest platform installed).
#
# A package that is checked with features names them in
# ANDROID_CHECK_FEATURES: interweave-human-android-platform's stand-ins are
# what its host tests run against, so they are checked for the phone too.
# CARGO overrides the cargo binary (the self-test's seam).
#
# Exit codes: 0 every package checks; 1 cargo check failed; 2 usage,
# environment or pins problem.
set -uo pipefail
HERE="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" && pwd )"
REPO="${ANDROID_CHECK_REPO:-$( cd -- "$HERE/../.." && pwd )}"
PINS="${ANDROID_TOOLCHAIN_PINS:-$REPO/tools/host/android/android-toolchain.pins}"
CARGO="${CARGO:-cargo}"
me="check_android_target"
die() { echo "$me: $*" >&2; exit 2; }

ANDROID_CHECK_PACKAGES=(interweave-profile-config interweave-transport-embedded
                        interweave-human-android-platform interweave-human-android)
ANDROID_CHECK_FEATURES=(interweave-human-android-platform/dev-stand-ins)

[[ -r "$PINS" ]] || die "cannot read the pins file $PINS"
pin() { awk -F= -v k="$1" '$1==k {print substr($0, index($0, "=") + 1); exit}' "$PINS"; }
target="$(pin RUST_ANDROID_TARGET)"
[[ "$target" =~ ^[a-z0-9_]+-linux-android$ ]] || die "RUST_ANDROID_TARGET in $PINS is '${target}', not a <arch>-linux-android triple"
ndk="$(grep -oE '^PKG_[0-9]+=ndk;[0-9.]+@' "$PINS" | head -n1 | sed 's/.*ndk;//; s/@$//')"
[[ -n "$ndk" ]] || die "no ndk;<version> package in $PINS"
api="$(grep -oE '^PKG_[0-9]+=platforms;android-[0-9]+@' "$PINS" | sed 's/.*android-//; s/@$//' | sort -n | head -n1)"
[[ -n "$api" ]] || die "no platforms;android-<n> package in $PINS"
compile_api="$(grep -oE '^PKG_[0-9]+=platforms;android-[0-9]+@' "$PINS" | sed 's/.*android-//; s/@$//' | sort -n | tail -n1)"

# The PINNED NDK, by its own source.properties: the SDK's ndk/<pin> first,
# ANDROID_NDK_HOME only if it is that same revision. GitHub's runner image
# exports ANDROID_NDK_HOME at the image's default NDK, so preferring it
# compiled with an NDK nobody pinned while the log named the pin.
revision() { awk -F' *= *' '$1=="Pkg.Revision" {print $2; exit}' "$1/source.properties" 2>/dev/null; }
ndk_home=""
for cand in ${ANDROID_HOME:+"$ANDROID_HOME/ndk/$ndk"} ${ANDROID_NDK_HOME:+"$ANDROID_NDK_HOME"}; do
    [[ "$(revision "$cand")" == "$ndk" ]] && { ndk_home="$cand"; break; }
done
if [[ -z "$ndk_home" ]]; then
    [[ -n "${ANDROID_HOME:-}${ANDROID_NDK_HOME:-}" ]] \
        || die "neither ANDROID_NDK_HOME nor ANDROID_HOME is set; the NDK $ndk is needed"
    seen=""
    [[ -n "${ANDROID_HOME:-}" ]] && seen="$ANDROID_HOME/ndk/$ndk is '$(revision "$ANDROID_HOME/ndk/$ndk")'"
    [[ -n "${ANDROID_NDK_HOME:-}" ]] && seen="${seen:+$seen; }ANDROID_NDK_HOME $ANDROID_NDK_HOME is '$(revision "$ANDROID_NDK_HOME")'"
    die "no NDK $ndk ($seen) — sdkmanager --install 'ndk;$ndk'"
fi
bin="$ndk_home/toolchains/llvm/prebuilt/linux-x86_64/bin"
arch="${target%%-*}"
clang="$bin/$arch-linux-android$api-clang"
[[ -x "$clang" ]] || die "no $clang in NDK $ndk at $ndk_home (an NDK without API $api's clang?)"
[[ -x "$bin/llvm-ar" ]] || die "no $bin/llvm-ar in NDK $ndk"

# Which packages the workspace has, by name, from cargo's own metadata.
have="$(cd "$REPO" && "$CARGO" metadata --no-deps --format-version 1 --locked 2>/dev/null \
        | python3 -c 'import json,sys; print("\n".join(p["name"] for p in json.load(sys.stdin)["packages"]))')" \
    || die "cargo metadata failed in $REPO"
pkgs=()
if (( $# )); then
    for p in "$@"; do grep -qx -- "$p" <<<"$have" || die "the workspace has no package '$p'"; pkgs+=("$p"); done
else
    for p in "${ANDROID_CHECK_PACKAGES[@]}"; do
        if grep -qx -- "$p" <<<"$have"; then pkgs+=("$p"); else echo "$me: $p is not in the workspace yet; not checked"; fi
    done
fi
(( ${#pkgs[@]} )) || die "none of ${ANDROID_CHECK_PACKAGES[*]} is in the workspace"

# What the Java-compiling build script under interweave-human-android needs.
for p in "${pkgs[@]}"; do
    [[ "$p" == interweave-human-android ]] || continue
    [[ -n "${JAVA_HOME:-}" && -x "$JAVA_HOME/bin/javac" ]] \
        || die "interweave-human-android needs a JDK: JAVA_HOME is '${JAVA_HOME:-}', with no bin/javac"
    [[ -n "${ANDROID_HOME:-}" ]] || die "interweave-human-android needs the SDK: ANDROID_HOME is not set"
    jar="$ANDROID_HOME/platforms/android-$compile_api/android.jar"
    [[ -f "$jar" ]] || die "interweave-human-android needs $jar (platforms;android-$compile_api)"
    export ANDROID_JAR="$jar"
    echo "$me: JDK at $JAVA_HOME, ANDROID_JAR at $jar"
done
features=()
for f in "${ANDROID_CHECK_FEATURES[@]}"; do
    for p in "${pkgs[@]}"; do [[ "${f%%/*}" == "$p" ]] && features+=("$f"); done
done
T="${target//-/_}"
export "CARGO_TARGET_${T^^}_LINKER=$clang" "CC_$T=$clang" "AR_$T=$bin/llvm-ar"
echo "$me: $target, NDK $ndk at $ndk_home, API $api: ${pkgs[*]}"
args=(); for p in "${pkgs[@]}"; do args+=(-p "$p"); done
(( ${#features[@]} )) && args+=(--features "$(IFS=,; echo "${features[*]}")")
(cd "$REPO" && "$CARGO" check --locked --target "$target" "${args[@]}") || { echo "$me: cargo check failed for $target" >&2; exit 1; }
echo "$me: OK — ${pkgs[*]} check for $target"
