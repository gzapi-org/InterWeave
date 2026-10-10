# android-e2e

Android ↔ desktop interoperability (plan §20 gate (c)): the orchestration between an Android peer and real desktop peers, a relay, process death and network transitions.

The Android side sits behind a seam, the `Device` trait in `src/lib.rs`: the lifecycle, and a named case run in the Android side's own process. The host never holds a binding to the Android side (architect-cto's DECISION of 2026-10-10). The cases are `interweave-android-e2e-cases` (`tests/android-e2e-cases`), one body for two runners:

- `HostStandIn` runs them on a thread over the embedded runtime the app's foreground service hosts (`interweave-transport-embedded`), started on this host under an app data directory of its own. It proves a case body before a phone runs it.
- `adb::AdbDevice` runs them in the app's instrumentation on a real device and reads the result from its status. Every case is a fresh app process; the phone reaches the relay and the desktop on this host's loopback through `adb reverse`; and the app's identity must survive a force-stop, which `AdbDevice::connect` checks. `src/adb.rs` says what such a run is and is not.

The desktop side is a `transport-daemon` from the shared harness (`interweave-test-support`, feature `e2e`), and its half of each case is here.

Running on a device: install the app's debug and androidTest builds, then

```sh
ANDROID_SERIAL=<serial> INTERWEAVE_ANDROID_INSTRUMENTATION=<test package>/<runner> \
  cargo test -p interweave-android-e2e-tests --test paths -- --ignored --test-threads 1
```

- `tests/stand_in.rs`: the stand-in itself. A kill releases the profile, and the restart serves the same `PeerId`.
- `tests/paths.rs`: a relayed and a direct path to a desktop daemon, both directions, the route-begin notices on each side, and the relayed side's process death and return; on two stand-ins, and (`--ignored`) with the phone as the relayed side and as the direct one. Its module note lists, by name, what it does not prove.
- `tests/human_chat.rs` (rust-ui-dev's cases): HumanChatV2 through each side's client, store and ui-model on the same topology, both directions on both paths, plain and `;ce=br`, the captured payloads validated against the envelope schema and the route indicator read at the ui-model. Its module note lists what it does not prove.

A run against the stand-in is evidence about the embedded composition, never about a phone. A run on a device is evidence about the phone's runtime over adb's tunnel, not over a radio network.

See [`architecture/docs/architecture/testing.md`](../../architecture/docs/architecture/testing.md) for normative scenarios and exit criteria.
