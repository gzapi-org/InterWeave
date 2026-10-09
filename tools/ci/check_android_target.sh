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
# when the workspace has it: interweave-transport-embedded joins on its own
# the day it lands, and until then the check is profile-config alone. A
# package NAMED on the command line that the workspace lacks is an error.
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
# Environment: ANDROID_NDK_HOME, or ANDROID_HOME with the NDK under
# ndk/<version> (the runner's SDK, after sdkmanager installs the pin).
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

ANDROID_CHECK_PACKAGES=(interweave-profile-config interweave-transport-embedded)

[[ -r "$PINS" ]] || die "cannot read the pins file $PINS"
pin() { awk -F= -v k="$1" '$1==k {print substr($0, index($0, "=") + 1); exit}' "$PINS"; }
target="$(pin RUST_ANDROID_TARGET)"
[[ "$target" =~ ^[a-z0-9_]+-linux-android$ ]] || die "RUST_ANDROID_TARGET in $PINS is '${target}', not a <arch>-linux-android triple"
ndk="$(grep -oE '^PKG_[0-9]+=ndk;[0-9.]+@' "$PINS" | head -n1 | sed 's/.*ndk;//; s/@$//')"
[[ -n "$ndk" ]] || die "no ndk;<version> package in $PINS"
api="$(grep -oE '^PKG_[0-9]+=platforms;android-[0-9]+@' "$PINS" | sed 's/.*android-//; s/@$//' | sort -n | head -n1)"
[[ -n "$api" ]] || die "no platforms;android-<n> package in $PINS"

ndk_home="${ANDROID_NDK_HOME:-${ANDROID_HOME:+$ANDROID_HOME/ndk/$ndk}}"
[[ -n "$ndk_home" ]] || die "neither ANDROID_NDK_HOME nor ANDROID_HOME is set; the NDK $ndk is needed"
bin="$ndk_home/toolchains/llvm/prebuilt/linux-x86_64/bin"
arch="${target%%-*}"
clang="$bin/$arch-linux-android$api-clang"
[[ -x "$clang" ]] || die "no $clang — is NDK $ndk installed at $ndk_home? (sdkmanager --install 'ndk;$ndk')"
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

T="${target//-/_}"
export "CARGO_TARGET_${T^^}_LINKER=$clang" "CC_$T=$clang" "AR_$T=$bin/llvm-ar"
echo "$me: $target, NDK $ndk, API $api: ${pkgs[*]}"
args=(); for p in "${pkgs[@]}"; do args+=(-p "$p"); done
(cd "$REPO" && "$CARGO" check --locked --target "$target" "${args[@]}") || { echo "$me: cargo check failed for $target" >&2; exit 1; }
echo "$me: OK — ${pkgs[*]} check for $target"
