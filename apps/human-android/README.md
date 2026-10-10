# human-android

The first-party Android human client (plan section 20): the native
library the APK loads, `interweave_human_android`, and the Gradle project
that packages it, [`android/`](android/README.md). The library's Activity
side draws the shared ui-slint views on Slint's NativeActivity backend;
its Service side hosts the embedded transport runtime
(`crates/transport/embedded`) through `crates/human/android-platform`. It
names no Slint crate itself: the toolkit is reached through ui-slint.

**Current status:** Stage 17 steps 2-4, first batch. The network
service hosts the runtime, store and facade; the Activity attaches to it;
unread arrivals raise a count-only notification. On a device the APK
draws the views and the service starts, and the embedded runtime does not
yet bind its listener there, so no session has run on a phone. The debug
APK runs on a stand-in profile and in-memory identity; a release build
starts nothing until step 6's Keystore key lands.

Build, on a host set up by `tools/host/android` (its README):

```sh
cd apps/human-android/android
echo "sdk.dir=$ANDROID_HOME" > local.properties   # not committed
PATH="$HOME/.cargo/bin:$PATH" JAVA_HOME=/opt/android-sdk/jdk ./gradlew assembleDebug
```

Dependency check, run locally by whoever changes the Gradle graph, before
the push that carries the change (plan section 20; no CI job runs it). It
reads the Gradle lockfiles against OSV.dev, so after a dependency change
regenerate them first:

```sh
cd apps/human-android/android
JAVA_HOME=/opt/android-sdk/jdk ./gradlew dependencies --write-locks   # only after a graph change
osv-scanner scan source --config=osv-scanner.toml -r .
```

It exits 0 when no known vulnerability is left, 1 when one is (the table
names each, with the lockfile it came from), and any other code for a
scan that did not run. `-r .` finds every module's `gradle.lockfile` and
the build's `buildscript-gradle.lockfile`; `--config` makes the one
[`osv-scanner.toml`](android/osv-scanner.toml) beside the build govern
them all, and that file says what an accepted finding's entry must carry.
The PR body says the check ran and what it found. osv-scanner 2.6.0 is
what this was written against.

