# android-e2e

Android ↔ desktop interoperability (plan §20 gate (c)): the orchestration between an Android peer and real desktop peers, a relay, process death and network transitions.

The Android side sits behind a seam, the `Device` trait in `src/lib.rs`: the lifecycle, and a named case run in the Android side's own process. No method of the seam yields a binding to the Android side (architect-cto's DECISION of 2026-10-10); the stand-in exposes its own beside it, for what the seam does not carry. A runner starts what a case needs: nothing, the runtime alone (`cases::NEED_A_RUNTIME`), or the app's service in full, whose hub it hands the case as the app's own client (`cases::NEED_THE_CLIENT`). The cases are `interweave-android-e2e-cases` (`tests/android-e2e-cases`), one body for two runners:

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
- `tests/human_chat.rs` (rust-ui-dev's cases): HumanChatV2 on the same topology, the Android side's half the cases crate's `human_chat`, run on the app's own client (its service host, store and hub, a draft typed and Send pressed through the model side) and the desktop's the desktop client's facade over IPC; both directions on both paths, plain and `;ce=br`, the captured payloads validated against the envelope schema and each route indicator read at the ui-model. On the host the Android sides are stand-ins running the app's `ServiceHost`; on a device (`--ignored`) the phone is one side. Its module note lists what it does not prove.
- `tests/platform_gates.rs`: plan §20 gate (d). The runtime root sits directly under the app data directory, wherever that path resolves. It and the profile's private directories are `0700`. A private directory under `files/` is refused by the runtime root, while the ancestor walk alone accepts it. Runs on the stand-in, and (`--ignored`) on a phone.
- `tests/audit_gate.rs`: plan §20 gate (g). With the profile at WARN, a trust set's audit record reaches the Android side's log, and no other line below WARN does. A first-party INFO and WARN line of the test's own pins the filter's level. It is a test binary of its own because the stand-in's log, `log_capture`, is a process-wide subscriber whose filter follows the running host's level, and another stand-in in the same process would move it.

A run against the stand-in is evidence about the embedded composition, never about a phone. A run on a device is evidence about the phone's runtime over adb's tunnel, not over a radio network.

See [`architecture/docs/architecture/testing.md`](../../architecture/docs/architecture/testing.md) for normative scenarios and exit criteria.
