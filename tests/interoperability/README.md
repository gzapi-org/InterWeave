# interoperability

Cross-version, cross-platform and wire compatibility, built in two parts.

**Stage 12 (this suite today)** — plan §15 (2), decided 2026-09-27. `tests/two_example_profiles.rs` composes two runtimes from two different shipped example profiles, `human-desktop.yaml` and `human-android.yaml`. Over real sockets they exchange:

- direct messages;
- broadcasts.

The frame vectors of both wires (`direct-v2/direct-message-v2-frame.json`, `gossipsub/broadcast-message-v1-frame.json`) carry their payload and media type through the composed runtimes, and arrive unchanged. So does the first vector's frozen message id.

The rest of each vector does not travel: its source and destination endpoints, its timestamp and its frame bytes. The composed runtime sends from the session's lease, to the receiver's configured endpoint, at its own time. The fingerprint, message-id and topic-key fixtures are not exercised here.

The byte-exact encoding of those vectors is pinned beside the codecs, in `tests/direct-v2` and `tests/pubsub`.

**Stage 17** — §20's platform tests:

- the desktop ↔ Android matrix on real devices;
- the upgrade matrices;
- independent codecs.

See [`architecture/docs/architecture/testing.md`](../../architecture/docs/architecture/testing.md) for the normative scenarios and exit criteria.
