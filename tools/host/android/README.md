# The Android toolchain on a host

Android work (Stage 17's SPIKE-008/009 first) needs the SDK, an NDK, a JDK
the Android Gradle Plugin accepts, and a Rust toolchain with the Android
target. One copy of the SDK side is installed per host at
`/opt/android-sdk`; the Rust side is per account. Every version is in
[`android-toolchain.pins`](android-toolchain.pins), and every host can be
checked against it.

| What | Version | Where |
|---|---|---|
| SDK platforms | API 30 (the test device), API 36 (Google Play's target from 2026-08-31) | `/opt/android-sdk/platforms/` |
| build-tools | 30.0.3, 36.0.0 (AGP 9.4's default) | `/opt/android-sdk/build-tools/` |
| NDK | 28.2.13676358 (r28c, AGP 9.4's default) | `/opt/android-sdk/ndk/` |
| platform-tools, cmdline-tools | 37.0.1, 23.0 | `/opt/android-sdk/` |
| JDK | Temurin 17.0.20.1+1 (AGP 9.4 needs 17) | `/opt/android-sdk/jdk` |
| Gradle | 9.6.0, through each project's wrapper | not installed; pin in `gradle-wrapper.properties` |
| rustup, Rust | rustup 1.29.1; the channel `rust-toolchain.toml` pins, + `aarch64-linux-android` | `~/.rustup`, `~/.cargo` |
| cargo-ndk | 4.1.2 | `~/.cargo/bin` |

## Using it

After the install (below), a login shell has:

```sh
ANDROID_HOME=/opt/android-sdk        # and ANDROID_SDK_ROOT, the same
ANDROID_NDK_HOME=/opt/android-sdk/ndk/28.2.13676358   # and ANDROID_NDK_ROOT
ANDROID_JDK_HOME=/opt/android-sdk/jdk
```

`JAVA_HOME` and `PATH` are left alone: the host's own JDK stays the
default, and a Gradle build names the pinned one, for example with
`org.gradle.java.home=/opt/android-sdk/jdk` in the project's
`gradle.properties`. The SDK's `adb` is not put on `PATH` either, since
two adb servers of different versions fight over the device. Use
`$ANDROID_HOME/platform-tools/adb` explicitly if you need the pinned one.

A Gradle wrapper pins `distributionUrl=…/gradle-9.6.0-bin.zip` with
`distributionSha256Sum=` the `GRADLE_DIST_SHA256` in the pins file.

## Installing it on a host (Qubes)

`/opt` is on the AppVM's root volume, which is discarded at shutdown, so
the install happens in the TemplateVM. The template has no network, so the
work is in two stages:

```sh
# 1. In an AppVM (has network), as any account: download, verify, stage.
bash tools/host/android/android-toolchain.sh --stage
#    It prints the qvm-copy-to-vm line for the archive and its .sha256.

# 2. In the TemplateVM, as root, from a checkout at the same commit:
sudo bash tools/host/android/android-toolchain.sh --install ~/QubesIncoming/<appvm>/android-toolchain-<id>.tar.gz
sudo usermod -aG androiddev <each Android account>
#    then shut the template down and restart the AppVM.
```

`--stage` accepts the SDK licences on behalf of the host, once. `--install`
refuses before writing anything:
- in an AppVM;
- on an archive that does not match its `.sha256`;
- on an archive staged from other pins than the checkout's.

Then each Android account, once:

```sh
bash tools/host/android/rust-android.sh
# and put ~/.cargo/bin before /usr/bin on PATH (the script checks, it does not edit)
```

## Checking a host

```sh
bash tools/host/android/android-toolchain.sh --check   # the shared install against the pins
bash tools/host/android/rust-android.sh --check        # this account's Rust side
```

Both are read-only. They exit 0 when everything matches and 1 naming each
difference. This is what to run on every host after a pin changes.

## Raising a version

Edit `android-toolchain.pins` (for an SDK package, the `PKG_<n>` path and
revision; Google's repository is
`https://dl.google.com/android/repository/repository2-3.xml`), then stage
and install again on every host. Until a host is re-installed, its
`--check` fails, naming the package. `--stage` fails if Google has moved a
package away from its pin, rather than staging something else.
