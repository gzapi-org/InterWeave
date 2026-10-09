# transport/embedded

The transport runtime hosted in-process: what the Android app's
foreground service starts, binds its clients to and stops (plan §20
step 1). It is the Stage 12 composition and its in-process
`LocalDataSession`/`LocalAdminPort` binding, not a second adapter, under
the trust boundary the platform supplies (ADR-0028 A 2026-10-08): the
app's data directory, with every private directory under one root the
host creates owner-only, `<app data dir>/interweave`.

No Android, JNI or UI type: it builds for the host and the device alike,
and its tests run on the host between real runtimes. The Service, its
lifecycle and the JNI side are `crates/human/android-platform`'s and
`apps/human-android`'s.

**Current status:** built (Stage 17); host-tested, the Android target
build not yet verified.
