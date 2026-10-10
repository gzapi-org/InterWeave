# android-e2e

Android ↔ desktop interoperability (plan §20 gate (c)): the orchestration between an Android peer and real desktop peers, a relay, process death and network transitions.

The Android side sits behind a seam, the `Device` trait in `src/lib.rs`. Until a target build and a device exist, it is `HostStandIn`: the embedded runtime the app's foreground service hosts (`interweave-transport-embedded`), started on this host under an app data directory of its own. The adb-driven device is to implement the same trait, so a case generic over it needs no change. The desktop side is a `transport-daemon` from the shared harness (`interweave-test-support`, feature `e2e`).

- `tests/stand_in.rs`: the stand-in itself. A kill releases the profile, and the restart serves the same `PeerId`.
- `tests/paths.rs`: a relayed and a direct path to a desktop daemon, both directions, the route-begin notices on each side, and the relayed side's process death and return. Its module note lists, by name, what it does not prove.
- `tests/human_chat.rs` (rust-ui-dev's cases): HumanChatV2 through each side's client, store and ui-model on the same topology, both directions on both paths, plain and `;ce=br`, the captured payloads validated against the envelope schema and the route indicator read at the ui-model. Its module note lists what it does not prove.

A run against the stand-in is evidence about the embedded composition, never about a phone.

See [`architecture/docs/architecture/testing.md`](../../architecture/docs/architecture/testing.md) for normative scenarios and exit criteria.
