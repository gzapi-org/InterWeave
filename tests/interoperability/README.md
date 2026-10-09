# interoperability

Cross-version, cross-platform and wire compatibility, built in two parts.

**Stage 12 (this suite today)** — plan §15 (2), decided 2026-09-27. `tests/two_example_profiles.rs` composes two runtimes from two different shipped example profiles, `human-desktop.yaml` and `human-android.yaml`. Over real sockets they exchange:

- direct messages;
- broadcasts.

The frame vectors of both wires (`direct-v2/direct-message-v2-frame.json`, `gossipsub/broadcast-message-v1-frame.json`) carry their payload and media type through the composed runtimes, and arrive unchanged. So does the first vector's frozen message id.

The rest of each vector does not travel: its source and destination endpoints, its timestamp and its frame bytes. The composed runtime sends from the session's lease, to the receiver's configured endpoint, at its own time. The fingerprint, message-id and topic-key fixtures are not exercised here.

The byte-exact encoding of those vectors is pinned beside the codecs, in `tests/direct-v2` and `tests/pubsub`.

**Stage 17** — §20's platform tests:

- **The independent codecs** (`tests/independent_codecs.rs`). Production is checked against `tests/independent-codecs`, which is written from the contract text with no path package in its graph. The checks cover the direct request and response frames, the broadcast envelope and the content fingerprint, over two thousand deterministic messages per shape, in both directions. Every one-byte mutation of a short frame must get the same verdict and the same fields from both decoders. The IPC envelope is compared on the frames HEAD's daemon writes in the IPC rows below.
- **The upgrade matrix, rows that need no older build** (testing.md §Compatibility fixtures, A 2026-10-08):

  | wire | unsupported major | minor bump | lower minor |
  |---|---|---|---|
  | IPC (`upgrade_matrix_ipc.rs`) | a major-3 hello gets exactly one frame, `close{VersionIncompatible, supported:[{2, IPC_MAX_MINOR}]}`, then EOF | offers at, above and far above HEAD's minor negotiate HEAD's and serve | the 2.0 golden frames, byte for byte, negotiate 2.0 and are served |
  | direct (`upgrade_matrix_direct.rs`) | a peer listing only 3.0.0: `UnsupportedProtocols` toward HEAD, `ProtocolUnsupported` from HEAD, nothing read | a peer listing `[2.1.0, 2.0.0]` exchanges on 2.0.0 in both dial directions | the frozen request and response vectors (`tests/direct-v2`, `tests/independent-codecs`) |
  | broadcast (`upgrade_matrix_broadcast.rs`) | versions 0, 2 and 255 from a trusted signed publisher are `Reject` and undelivered, beside version-1 controls | not applicable: the version byte is a major | the frozen envelope vectors |

  Every frame HEAD writes in these rows is read through the independent codecs. The raw peers stand in for a newer build and speak only those codecs.

- **The upgrade matrix's previous-build axis** (`upgrade_matrix_previous.rs`). Each build in `tools/ci/previous-builds.txt` (the first is `stage13-45ba3928`, the Stage 13 build that spoke IPC 2.0) is built from its own commit as a second binary and run against HEAD:

  - **The entry set:** the built entries under `INTERWEAVE_PREVIOUS_BUILDS` are exactly the list.
  - **HEAD's client on the old daemon:** a leased data session and an admin status.
  - **The old `transportctl` on HEAD's daemon:** `status` and `endpoints list`, with the sockets bound where HEAD's path rules put them.
  - **Peer to peer:** direct messages and broadcasts between the old daemon and HEAD, in both directions.

  The rows are `#[ignore]`. CI builds the list (devex-tooling's `tools/ci/build_previous_builds.sh`) and runs the file with `--ignored`, where an unset variable or a missing entry fails.

**Not yet proved:**

- **The desktop ↔ Android matrix** on real devices waits for the Android app.
- **Same-host rows only.** The direct and broadcast rows run on the host's private-range address, since ADR-0052 refuses a loopback peer address. They show nothing about NAT, relays or a second host.

See [`architecture/docs/architecture/testing.md`](../../architecture/docs/architecture/testing.md) for the normative scenarios and exit criteria.
