# Local IPC contract

Applies only because ADR-0015 selects a separate daemon.

## Transport choice

- Unix: Unix domain socket inside the profile runtime directory.
- Windows: named pipe with an owner-only ACL.
- Loopback TCP is not a default fallback; enabling it later requires a separate authentication design.

## Security boundary

The daemon creates the socket directory owner-only (`0700` on Unix) and the socket owner-only (`0600` equivalent where applicable). Peer credentials should be inspected where the OS exposes them. This prevents ordinary cross-user access; it does **not** protect against a malicious process already running as the same OS user.

Private identity keys never cross IPC.

## Framing

Each frame:

```text
4-byte unsigned big-endian length N
N bytes UTF-8 JSON object
```

- maximum frame: **65,536 bytes**;
- zero length is invalid;
- invalid UTF-8/JSON/version closes the offending client after a structured protocol error when possible;
- application payload bytes are base64url in JSON frames;
- strings and diagnostics have individual length caps.

The simplicity favors a future TypeScript/JavaScript Channel bridge talking to Rust without code generation. If profiling shows JSON encoding to be a bottleneck, a future IPC major version may use CBOR; the transport contract is unaffected.

## Handshake

Client first frame:

```json
{
  "type": "hello",
  "ipc_version": {"major": 1, "minor": 0},
  "client": {"kind": "claude-channel", "version": "..."},
  "requested_capabilities": ["events", "commands"]
}
```

Server replies with selected compatible version, transport contract version, profile identity, and capabilities. Major mismatch is fatal. Minor negotiation selects the lower supported compatible feature set.

## Message classes

- `request {id, method, params, deadline_ms?}`
- `response {id, ok, result? | error?}`
- `cancel {id}`
- `event {sequence, event_type, data}`
- `server_state {health,...}`

Request IDs are unique per connection. Event sequence is per IPC connection for diagnostics/gap detection only; it is not a durable replay cursor.

## Multiple clients

The daemon supports up to **16 local clients** by default. Each client has independent bounded command/event queues and subscription references. One slow client cannot backpressure the entire network event loop.

## Push events and overload

Each client event queue defaults to 256. When full:

1. drop oldest ordinary message events for that client;
2. preserve a small reserved lane for `OverloadObserved`, health, shutdown, and identity-change events;
3. increment drop counters;
4. never spill into an unbounded disk queue.

The bridge surfaces overload diagnostics but does not synthesize missing content.

## Disconnect/reconnect

A client disconnect releases ephemeral subscription references and outstanding response waiters. Reconnect performs a fresh handshake and resubscription. There is no event replay. A late response to a disconnected client is discarded after internal cleanup.

## Cancellation

Cancel is advisory. If an operation has crossed an irreversible network boundary, completion may race cancellation. Responses distinguish `CancelledBeforeDispatch` from `CancellationRaced` where observable.
