# Local IPC contract

Applies because ADR-0015 selects a separate daemon. Model B endpoint addressing makes this **IPC major version 2**.

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

- `N` maximum: **131,072 bytes (128 KiB)**; the 4-byte prefix is outside `N`;
- zero length is invalid;
- invalid UTF-8/JSON/version closes the offending client after a structured protocol error when possible;
- application payload bytes are base64url in JSON frames;
- strings and diagnostics have individual length caps.

### Payload-fit invariant

Every payload legal under the transport contract must be representable in both an IPC command and IPC `MessageReceived` event without exceeding the frame ceiling. 49,152 payload bytes expand to 65,536 base64url characters before JSON envelope overhead, so the 128 KiB body remains mandatory.

Phase 1 compatibility fixtures include both directions with exactly 49,152 opaque bytes and maximal bounded v2 metadata, including 64-byte source/destination EndpointIds.

Envelope limits:

- `media_type`: 128 ASCII bytes;
- `ChannelId`: 128 ASCII bytes;
- `EndpointId`: 64 ASCII bytes;
- normalized PeerId / transport identity string: 256 UTF-8 bytes;
- request/error diagnostic code: 128 ASCII bytes;
- human-readable diagnostic message: 2,048 UTF-8 bytes;
- client version string: 128 UTF-8 bytes.

## Handshake, endpoint claim, and client capabilities

Client first frame:

```json
{
  "type": "hello",
  "ipc_version": {"major": 2, "minor": 0},
  "client": {"kind": "human-client", "version": "..."},
  "endpoint": {"id": "human"},
  "requested_capabilities": ["events", "commands", "endpoints.query"],
  "features": ["keepalive"]
}
```

`endpoint` may be omitted only for a client that does not need direct send/receive, such as diagnostics/admin tooling.

Server validates endpoint claim before completing handshake. Phase 1 fixtures use these exact local error codes:

1. EndpointId grammar invalid -> `InvalidArgument`;
2. configured endpoint does not exist -> `EndpointUnknown`;
3. configured endpoint exists but is disabled -> `EndpointDisabled`;
4. optional `allowed_client_kinds` rejects the declared client kind -> `EndpointClientKindDenied`;
5. another live lease already owns the endpoint -> `EndpointInUse`;
6. requested capability or connection authorization is denied -> `CapabilityDenied`.

These are local IPC errors and intentionally more precise than the remote direct-protocol `no_route` privacy class. A remote peer never receives `EndpointUnknown`, `EndpointDisabled`, or `EndpointClientKindDenied`. If profile policy sets `ipc.keepalive.require_for_endpoint_lease=true`, a client that claims an EndpointId but did not negotiate `keepalive` is denied with `CapabilityDenied`; the daemon does not grant a lease first and revoke it later.

Server reply includes selected compatible IPC version, transport contract version, profile PeerId, caller endpoint (if any), a fresh local `endpoint_lease_epoch`, and granted capabilities. `endpoint_lease_epoch` is an opaque **128-bit lease-generation value** unique to that grant across reconnects and daemon restarts (for example random, or daemon-instance nonce + counter). It is not a bearer credential; it exists only to invalidate stale local route/reply state.

Endpoint lease is exclusive and connection-bound. Client cannot change EndpointId on an established IPC connection. Rebinding requires reconnect/new handshake.

### Client-kind warning

`client.kind` is not cryptographic authentication. It can prevent accidental configuration mistakes but cannot defend against malicious same-user code. Endpoint security still relies on profile configuration, owner-only IPC, exclusive leases, and any future stronger same-user authentication.

### Capabilities

- `events`: receive eligible runtime events;
- `commands`: ordinary non-administrative transport commands;
- `endpoints.query`: query a trusted remote peer's advertised endpoint directory;
- `admin.endpoints`: inspect/revoke local endpoint leases or mutate endpoint config through an administrative adapter;
- `admin.shutdown`: invoke transport `shutdown(grace)`.

`claude-channel` is never granted `admin.endpoints` or `admin.shutdown`. A human UI data-plane connection should likewise remain non-admin; its settings/control surface opens a separately authorized administrative connection.

Unknown/unauthorized capability requests are not silently elevated. Default grant policy is:

- `human-client`: `events`, `commands`, and `endpoints.query` when the profile endpoint-directory feature is enabled;
- `claude-channel`: `events` and `commands`; **not** `endpoints.query` by default;
- diagnostics clients: only explicitly configured read-only capabilities;
- administrative/control clients: administrative capabilities only when local policy explicitly grants them.

A future Claude `peer_endpoints` tool therefore requires an explicit capability-policy and tool-surface security review rather than inheriting access accidentally.

## Message classes

- `request {id, method, params, deadline_ms?}`
- `response {id, ok, result? | error?}`
- `cancel {id}`
- `event {sequence, event_type, data}`
- `server_state {health, connectivity?, ...}`

Request IDs are unique per connection. Event sequence is per IPC connection for diagnostics/gap detection only; it is not a durable replay cursor.

A request whose method requires an ungranted capability fails locally with a stable authorization error and is not dispatched to the transport runtime.

## Multiple clients

The daemon supports up to **16 IPC connections** by default. The limit counts connections, not applications: if a human application opens one data-plane connection and one separately authorized administrative connection, it consumes **two** client slots. Each connection has independent bounded command/event queues and subscription references. One slow client cannot backpressure the entire network event loop.

Each direct-capable client owns at most one EndpointId lease. Multiple local clients intentionally sharing a profile therefore use distinct endpoint IDs.

### Message-event routing

Local routing is normative:

- **broadcast `MessageReceived`:** enqueue only to connected IPC clients with `events` that currently hold a join reference for that ChannelId; `channels.desired` does not create local interest;
- **direct `MessageReceived`:** enqueue exactly one copy to the connected IPC client that owns the resolved `destination_endpoint` lease.

There is no first-client, round-robin, or all-client direct fan-out in IPC v2.

If the resolved endpoint is not leased, the daemon rejects the inbound direct request with coarse remote `no_route`. It does not acknowledge then drop, and it does not buffer for a future client.

## Local endpoint lease lifecycle

- lease grant produces local `EndpointLeaseChanged{registered}`;
- normal disconnect releases lease and all ephemeral join references;
- administrative revocation produces `EndpointLeaseChanged{revoked}` and stops direct routing immediately;
- endpoint configuration disable/reload revokes an active lease;
- a second live claim returns `EndpointInUse`;
- a reconnect receives a new lease epoch;
- no stale local reply route may authorize a new connection merely because it later claims the same EndpointId.

Bridge-local reply tokens disappear on bridge restart, so the Claude path naturally satisfies the stale-route rule. Other local apps must bind stored ephemeral reply routes to their current lease epoch.

## Direct command caller context

IPC `send` params contain remote destination and payload only:

```json
{
  "peer": "...",
  "endpoint": "human",
  "payload": "..."
}
```

`endpoint` here is the **remote destination endpoint** and may be omitted to request the remote default endpoint. There is no `source_endpoint` parameter. The daemon derives source from the caller's active lease.

A client without an endpoint lease receives `EndpointNotRegistered` for direct send.

## Push events and overload

Each client event queue defaults to 256. When full:

1. drop oldest ordinary broadcast events for that client as configured;
2. for an inbound direct message targeted at this endpoint, reject before transport `Accepted` if the event cannot be admitted;
3. preserve a reserved lane for overload, health, trust, endpoint-lease, shutdown, and identity events;
4. increment drop/rejection counters;
5. never spill into an unbounded disk queue.

## Disconnect/reconnect and optional keepalive

A client disconnect releases its EndpointId lease, ephemeral subscription references, and outstanding response waiters. Reconnect performs a fresh handshake and resubscription. There is no event replay. A late response to a disconnected client is discarded after internal cleanup.

IPC v2 also supports an optional negotiated liveness feature for detecting half-open/wedged clients that still hold endpoint leases:

```text
server -> ping { nonce }
client -> pong { nonce }
```

When enabled by profile policy and negotiated in `hello`, defaults are `interval=30s`, `response_timeout=10s`, `max_missed=3`. Only an exact matching pong satisfies the probe. After the configured miss threshold the daemon closes that IPC connection and releases its endpoint lease exactly as for an ordinary disconnect. Keepalive is local liveness detection only: it is not authentication, replay, a network heartbeat, or a lease-renewal credential.

The profile policy `ipc.keepalive.require_for_endpoint_lease` defaults to `true`. When true, any client that claims a data-plane EndpointId lease must negotiate keepalive during `hello`; otherwise endpoint claim fails with `CapabilityDenied`. Connections that do not claim an endpoint (for example a separate admin or diagnostics session) do not need keepalive solely because of this rule. Operators may set the policy false for compatibility with third-party clients, accepting that a half-open client may retain its lease until OS-level failure detection or explicit `admin.endpoints` revocation.

## Cancellation

Cancel is advisory. If an operation has crossed an irreversible network boundary, completion may race cancellation. Responses distinguish `CancelledBeforeDispatch` from `CancellationRaced` where observable.

## IPC v1 compatibility

There is no production v1 deployment requirement. The first production implementation targets IPC v2. If a future v1 adapter is added, it must be explicit and cannot reintroduce undocumented direct all-client fan-out into the v2 routing model.

## Connectivity status over IPC

When a client has ordinary read/status capability, `server_state` may include the backend-neutral `ConnectivitySummary` from the transport contract. Ordinary Claude/human data-plane clients receive only normalized direct/relay state and counts. Raw AutoNAT probe-server identities, relay PeerIds, relay multiaddrs, and server-capacity detail require a local diagnostics/admin capability and are never inferred as trust.

`ConnectivityChanged` is an operational event and may be coalesced on IPC to avoid state-flap event floods. It is not a durable replay stream and does not change endpoint lease semantics.
