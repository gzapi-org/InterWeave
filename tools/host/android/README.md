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

## Installing it on a host

The install is read-only to every account: every package is installed and
every licence accepted when it is staged, so a build only reads the SDK.
Root owns it, and no shared group is needed.

```sh
# 1. As any account, where there is network: download, verify, stage.
bash tools/host/android/android-toolchain.sh --stage
# 2. As root on the same host, from a checkout at the same commit:
sudo bash tools/host/android/android-toolchain.sh --install ~/android-staging/android-toolchain-<id>.tar.gz
```

**On a template-based Qubes AppVM** (`/qubes-vm-persistence` is
`rw-only`), `/opt` and `/etc` are discarded at shutdown.
`--install` keeps them with Qubes bind-dirs:
- the tree lives in `/rw/bind-dirs/opt/android-sdk`, on the persistent
  volume;
- `/rw/config/qubes-bind-dirs.d/50_android-sdk.conf` has Qubes bind-mount
  it onto `/opt/android-sdk`, and the profile onto
  `/etc/profile.d/android-sdk.sh`, at every boot;
- `--install` mounts both at once, so no restart is needed.

Where everything persists (a TemplateVM, a StandaloneVM, any host that is
not Qubes), `/opt/android-sdk` is written directly. The volume it lands on
needs room for the old tree, the new one and the archive at once (about
three times the archive's unpacked size) while the install runs.

The archive is treated as untrusted, since it was staged by an
unprivileged account and is unpacked as root. Before writing anything,
`--install` refuses:
- a DispVM, a Qubes VM whose persistence cannot be read, and one that
  persists neither its root nor `/rw`;
- a Python whose tarfile `data` filter has known bypasses (before 3.12.11
  or 3.13.4);
- an archive not matching its `.sha256`, or whose `.sha256` names another
  file;
- an archive staged from other pins than the checkout's;
- any member that is not a file, directory or symlink, a hard link, an
  absolute path, a `..` in a member name, a member reached through a
  symlink the archive makes, or a link out of the tree.

Run one `--install` at a time on a host: nothing serialises two.

Every member is judged before anything is written. Setuid and setgid bits
are dropped. A failed install leaves the previous
one in place, and an interrupted earlier swap is restored first. Every
pins value must match its form, and nothing in the file is ever executed.

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
