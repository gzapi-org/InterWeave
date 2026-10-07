#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
#
# Build the SPIKE-008 device harness APK by hand with the pinned toolchain
# only (tools/host/android/android-toolchain.pins), as SPIKE-009's: javac
# from the pinned JDK; aapt2, d8, zipalign and apksigner from build-tools
# 36.0.0; the Rust core through cargo-ndk. No Gradle.
#
# Compiled and linked against the API 36 platform, which the manifest's
# newer attributes need (dataExtractionRules, the remoteMessaging service
# type); minSdk 30; the TARGET SDK is the argument, so the same source is
# built at 30 and at Play's current target for the policy matrix (P1).
#
# Debuggable (`--debug-mode`) so results are read back with run-as. Signed
# with a throwaway TEST-ONLY key made in the build directory, never committed.
#
# The second argument is the backup posture, substituted for ALLOW_BACKUP in
# the manifest: false (standard v1, the default) or true (the rules alone).
#
#   bash spikes/spike-008/build.sh 30          # -> build/spike008-t30-nobackup.apk
#   bash spikes/spike-008/build.sh 30 true     # -> build/spike008-t30-backup.apk
#   bash spikes/spike-008/build.sh 36          # -> build/spike008-t36-nobackup.apk
set -euo pipefail

target="${1:?target SDK: 30 or 36}"
backup="${2:-false}"
case "$backup" in
    false) variant="t$target-nobackup" ;;
    true) variant="t$target-backup" ;;
    *) echo "backup posture: false or true, not $backup" >&2; exit 2 ;;
esac
here=$(cd "$(dirname "$0")" && pwd)
: "${ANDROID_HOME:=/opt/android-sdk}"
: "${ANDROID_JDK_HOME:=$ANDROID_HOME/jdk}"
export ANDROID_NDK_HOME="${ANDROID_NDK_HOME:-$ANDROID_HOME/ndk/28.2.13676358}"
tools="$ANDROID_HOME/build-tools/36.0.0"
platform="$ANDROID_HOME/platforms/android-36/android.jar"
out="$here/build"
work="$out/$variant"

rm -rf "$work"
mkdir -p "$work/classes" "$work/dex" "$work/stage/lib"

(cd "$here/harness" && cargo ndk -t arm64-v8a -P 30 -o "$work/stage/lib" build --release --locked)

"$tools/aapt2" compile --dir "$here/app/res" -o "$work/res.zip"
sed "s/ALLOW_BACKUP/$backup/" "$here/app/AndroidManifest.xml" > "$work/AndroidManifest.xml"
"$tools/aapt2" link -o "$work/unsigned.apk" -I "$platform" \
    --manifest "$work/AndroidManifest.xml" -R "$work/res.zip" \
    --min-sdk-version 30 --target-sdk-version "$target" \
    --version-code 1 --version-name "spike-$variant" --debug-mode

mapfile -t sources < <(find "$here/app/src" -name '*.java')
"$ANDROID_JDK_HOME/bin/javac" --release 11 -Xlint:-options \
    -classpath "$platform" -d "$work/classes" "${sources[@]}"
mapfile -t classes < <(find "$work/classes" -name '*.class')
"$tools/d8" --release --min-api 30 --lib "$platform" --output "$work/dex" "${classes[@]}"

(cd "$work/dex" && zip -q "$work/unsigned.apk" classes.dex)
(cd "$work/stage" && zip -q -r "$work/unsigned.apk" lib)
"$tools/zipalign" -f -p 4 "$work/unsigned.apk" "$work/aligned.apk"

if [ ! -f "$out/test-only.keystore" ]; then
    "$ANDROID_JDK_HOME/bin/keytool" -genkeypair -keystore "$out/test-only.keystore" \
        -storepass spike008 -keypass spike008 -alias spike008 -keyalg EC \
        -dname "CN=SPIKE-008 TEST-ONLY" -validity 30 >/dev/null 2>&1
fi
"$tools/apksigner" sign --ks "$out/test-only.keystore" --ks-pass pass:spike008 \
    --out "$out/spike008-$variant.apk" "$work/aligned.apk"
rm -f "$out/spike008-$variant.apk.idsig"
echo "$out/spike008-$variant.apk"
