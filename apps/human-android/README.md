# human-android

The first-party Android human client (plan section 20): the native
library the APK loads, `interweave_human_android`, and the Gradle project
that packages it, [`android/`](android/README.md). The library's Activity
side draws the shared ui-slint views on Slint's NativeActivity backend;
its Service side hosts the embedded transport runtime
(`crates/transport/embedded`) through `crates/human/android-platform`. It
names no Slint crate itself: the toolkit is reached through ui-slint.

**Current status:** Stage 17 steps 2-4 in progress. The APK builds and
draws the views on a device; the Service, its runtime and notifications
are not wired yet.

Build, on a host set up by `tools/host/android` (its README):

```sh
cd apps/human-android/android
echo "sdk.dir=$ANDROID_HOME" > local.properties   # not committed
PATH="$HOME/.cargo/bin:$PATH" JAVA_HOME=/opt/android-sdk/jdk ./gradlew assembleDebug
```
