# interoperability

Cross-version, cross-platform and wire compatibility, built in two parts.

**Stage 12 (this suite today)** — plan §15 (2), decided 2026-09-27. `tests/two_example_profiles.rs` composes two runtimes from two different shipped example profiles, `human-desktop.yaml` and `human-android.yaml`. Over real sockets they exchange:

- direct messages;
- broadcasts.

Every frozen `fixtures/` vector for both wires (`direct-v2`, `gossipsub`) travels through the composed runtimes and arrives unchanged.

The byte-exact encoding of those vectors is pinned beside the codecs, in `tests/direct-v2` and `tests/pubsub`.

**Stage 17** — §20's platform tests:

- the desktop ↔ Android matrix on real devices;
- the upgrade matrices;
- independent codecs.

See [`architecture/docs/architecture/testing.md`](../../architecture/docs/architecture/testing.md) for the normative scenarios and exit criteria.
