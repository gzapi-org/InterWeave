# ipc-client

The desktop IPC v2 client: the neutral local-session binding over the daemon's data and admin Unix sockets.

**Current status:** Stage 13, active workspace member. `IpcBinding` implements `DataSessionBinding` and `AdminBinding` (`interweave-local-client-api`), so a local application holds the same `DataSessionPort` and `AdminPort` whether its binding is in-process or IPC; `tests/local-client-conformance` runs the same generic functions against both. Like the server, it depends on nothing under `crates/transport/*` and on no libp2p crate. Unix only.

## What differs from the in-process binding

All three are of the wire, and `LOCAL-IPC.md` names them (A 2026-09-30):

- Events are pushed, so `events` returns what has arrived; one the daemon admitted may still be on its way.
- The receive buffer is bounded at the granted `event_queue`. A full buffer stops the client reading its socket, so a response queued behind undrained events waits for them, and past the keepalive miss threshold the daemon closes the connection as wedged. Draining events is part of holding a lease.
- The grouped order of `events` (session notices, then direct, then broadcast) holds within one server pump; across pumps batches are read as they arrive, so a notice pumped after a direct message follows it.

## What it does not decide

Every refusal is the daemon's answer, carried back as its code. The one read that never reaches the daemon is `events`, which takes from what was already pushed; its capability is judged locally, as the in-process binding judges it.
