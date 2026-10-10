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

The profile's identity at rest is `custody` (plan §20 step 6, ADR-0042
A 2026-10-10): the exact Ed25519 seed sealed by the platform's cipher
into the IWK1 v1 envelope and stored with its PeerId as one owner-only
record, `identity.iwk1`, in the identity directory. The cipher is a
seam, `SeedCipher`, with a closed failure set; on a device it is the
Keystore adapter `apps/human-android` implements. `unlock` gives the
identity `EmbeddedLaunch` takes, or why not, and never makes a new
one; `provision` and `restore` seal under the profile lock. Its tests
(`tests/custody.rs`) run on the host with a software AES-256-GCM
cipher; the Keystore itself is the device test's.

**Current status:** built (Stage 17); host-tested, the Android target
build not yet verified.
