# local-client-conformance

The same LocalDataSession/LocalAdminPort conformance suite runs against every platform binding: the direct in-process one (Stage 12, the Android embedded adapter's core), the desktop IPC one (Stage 13), and the in-memory fake the human client is built against (Stage 14, `tests/local-client-fake`).

The checks are generic functions over the neutral binding traits in `interweave-local-client-api` (`DataSessionBinding`, `DataSessionPort`, and for administration `AdminBinding`, `AdminPort`), in `src/lib.rs`, one per item of [`contracts/LOCAL-CLIENT.md`](../../architecture/contracts/LOCAL-CLIENT.md) §7. A binding passes by running each against a sender and a receiver it composed:

- `tests/in_process.rs` — the in-process binding, over two runtimes composed from profiles and connected over real sockets on the host's private address.
- `tests/over_ipc.rs` — the IPC adapter: `ipc-client` to `ipc-server` to the in-process binding (Stage 13).
- `tests/fake.rs` — the in-memory fake: the same generic functions (Stage 14, plan §17 (2)). What a fake cannot honour is `tests/local-client-fake/README.md`'s to say.

Items 9 and 10 (A 2026-10-03, #175) are generic too: `ready()` resolves on what waits and takes nothing, beside the direct message that is its positive control, and a session is owed one `ServerState` at open and no second without a change. Several changes coalescing into one pending state needs a binding to drive a change, so each binding shows it with its own means: the fake through `set_health` in `tests/fake.rs`, the in-process registry in its unit tests, `ipc-client` against a scripted server. The in-process runner also checks that its `ready()` is woken -- by a delivery, a revocation and the stop -- well inside its one-second recheck, so a test cannot pass on the recheck alone.

Item 7's structural half is the traits themselves: a data session has no method yielding administrative authority. Its runtime half -- the admin port's own capabilities, the notice a revocation owes the holder, and the runtime overlay (disabling revokes and never rebinds; a default must be able to receive) -- is generic over `AdminBinding` since Stage 13 (plan §16 (2)), so every binding runs it. What only one binding can show stays beside it: the in-process runner checks that an ended or cancelled session leaves nothing held, through the runtime's own diagnostics.

See [`architecture/docs/architecture/testing.md`](../../architecture/docs/architecture/testing.md) for the normative scenarios and exit criteria.
