#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# Build the SPIKE-009 device harness APK by hand, with the pinned toolchain
# only (tools/host/android/android-toolchain.pins): javac from the pinned
# JDK, d8, aapt2, zipalign and apksigner from build-tools 36.0.0, and the
# Rust core through cargo-ndk. No Gradle, so nothing is fetched and nothing
# beyond the pins file needs pinning.
#
# The APK is debuggable (`--debug-mode`) so its results can be read back
# with `run-as`; that changes nothing AndroidKeyStore enforces.
#
# The APK is signed with a throwaway key made in the build directory on
# first use: TEST-ONLY, never committed, worth nothing outside this harness.
#
#   bash spikes/spike-009/device/build.sh        # -> build/spike009.apk
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
: "${ANDROID_HOME:=/opt/android-sdk}"
: "${ANDROID_JDK_HOME:=$ANDROID_HOME/jdk}"
export ANDROID_NDK_HOME="${ANDROID_NDK_HOME:-$ANDROID_HOME/ndk/28.2.13676358}"
tools="$ANDROID_HOME/build-tools/36.0.0"
platform="$ANDROID_HOME/platforms/android-30/android.jar"
out="$here/build"

rm -rf "$out/classes" "$out/dex" "$out/stage"
mkdir -p "$out/classes" "$out/dex" "$out/stage/lib"

(cd "$here/harness" && cargo ndk -t arm64-v8a -P 30 -o "$out/stage/lib" build --release --locked)

mapfile -t sources < <(find "$here/app/src" -name '*.java')
"$ANDROID_JDK_HOME/bin/javac" --release 11 -Xlint:-options \
    -classpath "$platform" -d "$out/classes" "${sources[@]}"
mapfile -t classes < <(find "$out/classes" -name '*.class')
"$tools/d8" --release --min-api 30 --lib "$platform" --output "$out/dex" "${classes[@]}"

"$tools/aapt2" link -o "$out/unsigned.apk" -I "$platform" \
    --manifest "$here/app/AndroidManifest.xml" \
    --min-sdk-version 30 --target-sdk-version 30 \
    --version-code 1 --version-name spike --debug-mode
(cd "$out/dex" && zip -q "$out/unsigned.apk" classes.dex)
(cd "$out/stage" && zip -q -r "$out/unsigned.apk" lib)
"$tools/zipalign" -f -p 4 "$out/unsigned.apk" "$out/aligned.apk"

if [ ! -f "$out/test-only.keystore" ]; then
    "$ANDROID_JDK_HOME/bin/keytool" -genkeypair -keystore "$out/test-only.keystore" \
        -storepass spike009 -keypass spike009 -alias spike009 -keyalg EC \
        -dname "CN=SPIKE-009 TEST-ONLY" -validity 30 >/dev/null 2>&1
fi
"$tools/apksigner" sign --ks "$out/test-only.keystore" --ks-pass pass:spike009 \
    --out "$out/spike009.apk" "$out/aligned.apk"
rm -f "$out/unsigned.apk" "$out/aligned.apk" "$out/spike009.apk.idsig"
echo "$out/spike009.apk"
