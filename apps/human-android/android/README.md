# Android packaging

The Gradle build of the Android human client: one app module, `app/`,
whose `cargoNdkDebug`/`cargoNdkRelease` tasks build the native library
(`apps/human-android`) with cargo-ndk before it is packaged. Kotlin here
is the platform's glue only: the Activity and the foreground Service hand
their lifecycle to the native library, and domain logic, retention,
transport policy and crypto stay in Rust crates.

Every plugin and library version is in `gradle/libs.versions.toml`;
Gradle itself is pinned by the wrapper, whose distribution checksum is the
one in `tools/host/android/android-toolchain.pins`.

Android instrumented tests belong under `app/src/androidTest/`;
host-orchestrated Android/network E2E scenarios belong in
`tests/android-e2e/`.
