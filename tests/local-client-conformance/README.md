# local-client-conformance

The same LocalDataSession/LocalAdminPort conformance suite runs against every platform binding: the direct in-process one (Stage 12, the Android embedded adapter's core) and the desktop IPC one (Stage 13).

The checks are generic functions over the neutral binding traits in `interweave-local-client-api` (`DataSessionBinding`, `DataSessionPort`, and for administration `AdminBinding`, `AdminPort`), in `src/lib.rs`, one per item of [`contracts/LOCAL-CLIENT.md`](../../architecture/contracts/LOCAL-CLIENT.md) §7. A binding passes by running each against a sender and a receiver it composed:

- `tests/in_process.rs` — the in-process binding, over two runtimes composed from profiles and connected over real sockets on the host's private address.
- Stage 13 adds the IPC adapter's runner over the same functions.

Item 7's structural half is the traits themselves: a data session has no method yielding administrative authority. Its runtime half -- the admin port's own capabilities, the notice a revocation owes the holder, and the runtime overlay (disabling revokes and never rebinds; a default must be able to receive) -- is generic over `AdminBinding` since Stage 13 (plan §16 (2)), so every binding runs it. What only one binding can show stays beside it: the in-process runner checks that an ended or cancelled session leaves nothing held, through the runtime's own diagnostics.

See [`architecture/docs/architecture/testing.md`](../../architecture/docs/architecture/testing.md) for the normative scenarios and exit criteria.
