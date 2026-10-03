# Local IPC contract

Applies because ADR-0015 selects a separate daemon. Model B endpoint addressing makes this **IPC major version 2**.

The prose here is normative for **behaviour**. The shapes it describes are also defined as JSON Schema under [`schemas/ipc/`](./schemas/ipc/) — normative for **shape**, carrying an `x-contract.status` that says what each is authoritative about (ADR-0049). Where the two describe the same field they must agree; a disagreement is a bug in whichever one drifted.

## Transport choice and authority domains

IPC v2 uses **two distinct local endpoints**:

- data-plane socket: Unix domain socket `<runtime>/<profile>.sock`; Windows named-pipe equivalent;
- administrative socket: Unix domain socket `<runtime>/<profile>.admin.sock` — the `.` sits outside the profile-name alphabet (`[A-Za-z0-9_-]`), so no profile's data socket can share a path with another's admin socket (A 2026-10-01); Windows named-pipe equivalent.

Loopback TCP is not a default fallback; enabling it later requires a separate authentication design. The socket selected by the client is an authority-domain input: the data-plane socket can never grant `admin.*`, regardless of `client.kind` or requested capability names. The admin socket never grants an EndpointId lease and is not used for ordinary direct/broadcast message delivery.

## Security boundary

The daemon creates the runtime directory owner-only (`0700` on Unix) and both sockets owner-only (`0600` equivalent where applicable) by default. Peer credentials are a **MUST** on Unix (§Peer identity, lock and stale sockets): the peer uid equals the runtime directory's owner uid, or the connection is closed before `hello` (A 2026-09-28). Deployments may apply a stricter owner/group/service-account ACL to the admin socket than to the data socket. The split is an enforceable protocol/capability boundary against client-kind spoofing and accidental privilege crossover, but default same-UID filesystem permissions still do **not** protect against a malicious process already running as the same OS user. Strong same-user executable/user-presence authentication remains SPIKE-005 territory.

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

Phase 1 compatibility fixtures include both directions with exactly 49,152 opaque bytes and maximal bounded v2 metadata, including 64-character source/destination EndpointIds.

Envelope limits:

- `media_type`: 128 characters;
- `ChannelId`: 128 characters;
- `EndpointId`: 64 characters;
- normalized PeerId / transport identity string: the `common/peer-id`
  grammar (46 or 52 characters); the 256 an implementation may reserve
  (`TransportIdentity::MAX_BYTES`) is a code ceiling, not a schema bound;
- request/error diagnostic code: 128 characters;
- human-readable diagnostic message: 2,048 characters;
- client version string: 128 characters.

Every bound on a string in this contract is in **characters** — Unicode
code points, the unit JSON Schema's `maxLength` counts — so the schema
is the one authority for it and this document states no second unit
(A 2026-09-29). For the ASCII-patterned fields characters and bytes
coincide; for every string without an ASCII pattern (the diagnostic
message, the client version and kind, feature names, the request id,
`reason_class`) a maximal value occupies at most four bytes per code
point as raw UTF-8 — 8 KiB for the message — and at most twelve when
a writer escapes every code point (`\uD83D\uDE00`), 24 KiB, both inside
the 128 KiB frame ceiling, which remains the only bound in bytes. An
implementation sends and reads by the character count; the Rust
mirrors that still count bytes — the client version, the client kind on
the paths that bound it, feature names, and the message's send-side cut
(`Close::with_message`) — move to characters on the B1 pull request
(#147), where the request id, `reason_class` and the message's read
side already did.

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

On the data-plane socket `endpoint` may be omitted. A connection that omits it holds no lease: it may hold `commands` and `events`, join, leave and publish; its `direct.send` is answered `EndpointNotRegistered` at the port, before the network, and no direct message is ever routed to it — the non-spoofable source of ADR-0030 is derived from the lease at the send, so a session without one has no source, not a forged one. A read-only diagnostics client is the same shape with `events` alone (A 2026-09-30). Administrative clients connect to the separate admin socket and MUST omit endpoint claims; the admin socket never owns an EndpointId lease.

Server validates endpoint claim before completing handshake. Phase 1 fixtures use these exact local error codes:

1. EndpointId grammar invalid -> `InvalidArgument`;
2. configured endpoint does not exist -> `EndpointUnknown`;
3. configured endpoint exists but is disabled -> `EndpointDisabled`;
4. optional `allowed_client_kinds` rejects the declared client kind -> `EndpointClientKindDenied`;
5. another live lease already owns the endpoint -> `EndpointInUse`;
6. requested capability or connection authorization is denied -> `CapabilityDenied`.

The list names the codes, not the order they are judged in. The order
(A 2026-10-01, from proving the Stage 13 deferrals): on the data socket
the claim's grammar is read first (item 1), then the capability checks
that need no lease — `admin.*` requested on the data socket, or a claim
without `keepalive` where `require_for_endpoint_lease` holds, each
`CapabilityDenied` (item 6) — and only then the binding's claim: items
2, 3, 4 and 5 in that order; the capabilities themselves are then
judged per request, not at the handshake (`CapabilityDenied` on a
request a session's grant does not cover). So a hello claiming a malformed id and asking for
`admin.*` is `InvalidArgument`, and a well-formed claim of an absent
endpoint that omits `keepalive` is `CapabilityDenied`, not
`EndpointUnknown`. On the admin socket any endpoint claim within the hello's
length bounds is refused as `CapabilityDenied` before its grammar is
read, since no lease is ever
granted there (§Transport choice and authority domains). The claim's
id travels as the client wrote it (`ipc/hello` 1.2.0): the schema and
the Rust mirror bound its length alike (1 to 64 characters, a framing
error outside them on both), and within the bounds the handshake judges
its grammar, so a schema-driven server and the Rust mirror answer the
same code for the same well-formed JSON.

These are local IPC errors and intentionally more precise than the remote direct-protocol `no_route` privacy class. A remote peer never receives `EndpointUnknown`, `EndpointDisabled`, or `EndpointClientKindDenied`. If profile policy sets `ipc.keepalive.require_for_endpoint_lease=true`, a client that claims an EndpointId but did not negotiate `keepalive` is denied with `CapabilityDenied`; the daemon does not grant a lease first and revoke it later.

Server reply includes selected compatible IPC version, transport contract version, profile PeerId, caller endpoint (if any), a fresh local `endpoint_lease_epoch`, the granted event queue bound (`event_queue`, present with `endpoint`), and granted capabilities. `endpoint_lease_epoch` is an opaque **128-bit lease-generation value** unique to that grant across reconnects and daemon restarts (for example random, or daemon-instance nonce + counter). It is not a bearer credential; it exists only to invalidate stale local route/reply state.

Endpoint lease is exclusive and connection-bound. Client cannot change EndpointId on an established IPC connection. Rebinding requires reconnect/new handshake.

### Client-kind warning

`client.kind` is not cryptographic authentication. It can prevent accidental endpoint misbinding but cannot select the administrative authority domain. A client claiming `kind: transportctl` on the data-plane socket is still categorically ineligible for `admin.*`; a client on the admin socket is ineligible for EndpointId leases/data-plane messaging. Same-user code that can open the admin socket remains inside the documented residual boundary unless the deployment applies stricter OS ACLs or future SPIKE-005 authentication.

### Capabilities

- `events`: receive eligible runtime events;
- `commands`: the data-domain methods of the catalogue (`channel.join`, `channel.leave`, `broadcast.publish`, `direct.send`); connectivity reaches a data client only as the normalized `server_state.connectivity` push, never as a method;
- `endpoints.query`: query a trusted remote peer's advertised endpoint directory;
- `admin.status`: read the administrative status view (`admin-status`: health, the full connectivity summary, counters, lease count) — read-only, admin socket only (A 2026-09-28); `ipc.events_dropped_total` is emitted only by a binding that keeps a per-client drop count; while none does, the member is omitted — a counter the server cannot keep is absent, never `0` (A 2026-09-29).
- `admin.endpoints`: inspect/revoke local endpoint leases or mutate the endpoint runtime overlay (enable/disable, default) through an administrative adapter;
- `admin.shutdown`: invoke transport `shutdown(grace)`.
- `admin.trust` (2.1, A 2026-10-03): read the profile's peer trust policy and mutate it through an administrative adapter — admin socket only, never on the data socket under any `client.kind` (ADR-0037 A 2026-10-03; ADR-0032: trust mutation requires the platform admin binding).

`claude-channel` is never granted `admin.endpoints` or `admin.shutdown`. A human UI data-plane connection is likewise non-admin; its settings/control surface opens the separate administrative socket. The data-plane socket rejects every `admin.*` request with `CapabilityDenied` before dispatch even if `client.kind` claims an administrative name.

Unknown/unauthorized capability requests are not silently elevated. Default grant policy is:

- `human-client`: `events`, `commands`, and `endpoints.query` when the profile endpoint-directory feature is enabled;
- `claude-channel`: `events` and `commands`; **not** `endpoints.query` by default;
- diagnostics clients: only explicitly configured read-only capabilities;
- administrative/control clients: `admin.*` capabilities only on the admin socket and only when local policy/OS access permits the administrative connection; the admin socket does not grant `events`/`commands` for application messaging or EndpointId leases.

A future Claude `peer_endpoints` tool therefore requires an explicit capability-policy and tool-surface security review rather than inheriting access accidentally.

## Message classes

- `hello` (the client's first frame) and `hello_response` (server-only)
- `close {code, message?, supported?}` (server-only; connection-fatal)
- `request {id, method, params, deadline_ms?}` — `method` from the closed catalogue below
- `response {id, ok, result? | error?}`
- `cancel {id}`
- `event {sequence, event_type, data}` — `event_type` from the closed event catalogue below
- `server_state {health, connectivity?}`
- `ping {nonce}` / `pong {nonce}`

One envelope, [`schemas/ipc/frame.schema.json`](./schemas/ipc/frame.schema.json) 2.0.0, covers all ten classes.

Request IDs are unique per connection. A request whose id is still outstanding on the connection — in flight or waiting — is a protocol violation: the server answers `close{ProtocolViolation}` and closes, since no response bearing that id could be told from the first's (A 2026-09-29). Event sequence is per IPC connection for diagnostics/gap detection only; it is not a durable replay cursor.

A request whose method requires an ungranted capability fails locally with a stable authorization error and is not dispatched to the transport runtime.

## Multiple clients

The daemon supports up to **16 IPC connections total** by default across both sockets, with at most **4 admin-socket connections** by default. The limit counts connections, not applications: if a human application opens one data-plane connection and one administrative connection, it consumes **two** total slots. Each connection has independent bounded request/event state. One slow client cannot backpressure the entire network event loop. A connection from the owner's uid accepted past either limit is answered `close{Overloaded}` before any hello is read; nothing about it is read first, and the peer-credential check of §Peer identity comes before the limits, so a foreign uid is still closed silently (A 2026-09-29).

Each direct-capable client owns at most one EndpointId lease. Multiple local clients intentionally sharing a profile therefore use distinct endpoint IDs.

### Message-event routing

Local routing is normative:

- **broadcast `MessageReceived`:** enqueue only to connected IPC clients with `events` that currently hold a join reference for that ChannelId; `channels.desired` does not create local interest;
- **direct `MessageReceived`:** enqueue exactly one copy to the connected IPC client that owns the resolved `destination_endpoint` lease.

There is no first-client, round-robin, or all-client direct fan-out in IPC v2.

If the resolved endpoint is not leased, the daemon rejects the inbound direct request with coarse remote `no_route`. It does not acknowledge then drop, and it does not buffer for a future client.

## Local endpoint lease lifecycle

- a lease grant is learned from `hello_response` (endpoint + `endpoint_lease_epoch`); no event announces it;
- normal disconnect releases lease and all ephemeral join references;
- administrative revocation, or the endpoint being disabled, produces `endpoint.lease_changed {endpoint, revoked_epoch}` and stops direct routing immediately;
- endpoint configuration disable/reload revokes an active lease;
- a second live claim returns `EndpointInUse`;
- a reconnect receives a new lease epoch;
- no stale local reply route may authorize a new connection merely because it later claims the same EndpointId.

Bridge-local reply tokens disappear on bridge restart, so the Claude path naturally satisfies the stale-route rule. Other local apps must bind stored ephemeral reply routes to their current lease epoch.

## Direct command caller context

IPC `direct.send` params (`send-params` 2.0.0) carry the remote destination, the caller's message identity and the payload:

```json
{
  "peer": "...",
  "endpoint": "human",
  "message_id": "...",
  "payload": {"media_type": "text/plain", "bytes": "..."}
}
```

`endpoint` here is the **remote destination endpoint** and may be omitted to request the remote default endpoint. `message_id` is the caller's and REQUIRED (send-params 2.0.0): a retry after a lost response must carry the same identity or dedup cannot recognise it. There is no `source_endpoint` parameter. The daemon derives source from the caller's active lease.

A client without an endpoint lease receives `EndpointNotRegistered` for direct send.

## Push events and overload

Each client event queue defaults to 256. When full:

1. drop oldest ordinary broadcast events for that client as configured;
2. for an inbound direct message targeted at this endpoint, reject before transport `Accepted` if the event cannot be admitted;
3. preserve a reserved lane for `server_state`, `endpoint.lease_changed`, `peer.disconnected` and `close`, which are never dropped in favour of ordinary broadcast events;
4. increment drop/rejection counters;
5. never spill into an unbounded disk queue.

Over IPC the server pumps the session queue into its event lane and the socket, and the client into its own bounded buffer, so what a sender can get accepted while the reader does not drain is the whole pipeline's capacity: the session queue, the event lane, the client's buffer, and the socket — whose share is the kernel's send buffer, bounded in bytes, not events, and therefore hundreds of small frames or a handful of large ones. Bounded, larger than one `event_queue`, and no number this contract states. Acceptance still follows admission at the session queue and every accepted message is held and delivered; nothing is buffered anywhere a bound does not name (A 2026-09-30).

Event order over IPC: within one server pump the grouped order of `events()` holds (session notices, then direct, then broadcast, each oldest first); across pumps the client reads batches as they arrive, so a notice pumped after a direct message follows it. A consumer that needs one order across a session uses the receipt times a direct message and a broadcast carry; a notice carries none and is read as of its arrival (A 2026-09-30).

## Disconnect/reconnect and optional keepalive

A client disconnect releases its EndpointId lease, ephemeral subscription references, and outstanding response waiters. Reconnect performs a fresh handshake and resubscription. There is no event replay. A late response to a disconnected client is discarded after internal cleanup.

IPC v2 also supports an optional negotiated liveness feature for detecting half-open/wedged clients that still hold endpoint leases:

```text
server -> ping { nonce }
client -> pong { nonce }

nonce = 128-bit CSPRNG value, encoded canonically (for example base64url without padding)
```

When enabled by profile policy and negotiated in `hello`, defaults are `interval=30s`, `response_timeout=10s`, `max_missed=3`. The server has at most one outstanding keepalive nonce per connection; only an exact pong for the current 128-bit nonce satisfies the probe. Stale/duplicate/wrong nonces do not reset liveness state. After the configured miss threshold the daemon closes that IPC connection and releases its endpoint lease exactly as for an ordinary disconnect. Keepalive is local liveness detection only: it is not authentication, replay protection for application messages, a network heartbeat, or a lease-renewal credential.

The profile policy `ipc.keepalive.require_for_endpoint_lease` defaults to `true`. When true, any client that claims a data-plane EndpointId lease must negotiate keepalive during `hello`; otherwise endpoint claim fails with `CapabilityDenied`. Connections that do not claim an endpoint (for example a separate admin or diagnostics session) do not need keepalive solely because of this rule. Operators may set the policy false for compatibility with third-party clients, accepting that a half-open client may retain its lease until OS-level failure detection or explicit `admin.endpoints` revocation.

An IPC client's receive buffer is bounded at its granted `event_queue` plus the one event its reader holds while it pauses; a client whose buffer is full stops reading its socket, so responses wait behind undrained events and, past the keepalive miss threshold, the server closes it as wedged. Draining events is part of holding a lease (A 2026-09-30).

## Cancellation

Cancel is advisory. If an operation has crossed an irreversible network boundary, completion may race cancellation. Responses distinguish `CancelledBeforeDispatch` from `CancellationRaced` where observable.

## Method catalogue

Every request names one method of the closed catalogue
[`schemas/ipc/method.schema.json`](./schemas/ipc/method.schema.json);
[`schemas/ipc/request.schema.json`](./schemas/ipc/request.schema.json)
binds each name to its params shape. A name outside the catalogue is
answered `ProtocolUnsupported` and the connection stays; a name of the
admin domain arriving on the data socket is answered `CapabilityDenied`
before dispatch, whatever the client's kind, and counted
(`ipc_cross_domain_capability_denied_total`). The capability a method
needs is this table's and the mirror table in `crates/api/ipc-protocol`;
the schema-agreement test binds the two.

| Method | Domain | Capability | Params | Result | Since |
|---|---|---|---|---|---|
| `channel.join` | data | `commands` | `channel-params` | `empty-result` | 2.0 |
| `channel.leave` | data | `commands` | `channel-params` | `empty-result` | 2.0 |
| `broadcast.publish` | data | `commands` | `publish-params` | `empty-result` | 2.0 |
| `direct.send` | data | `commands` (a lease required: `EndpointNotRegistered` otherwise) | `send-params` | `send-result` | 2.0 |
| `endpoints.query` | data | `endpoints.query` | `query-params` | `endpoints:directory-response` | 2.0 |
| `admin.status` | admin | `admin.status` | none | `admin-status` | 2.0 |
| `admin.endpoints.list` | admin | `admin.endpoints` | none | `endpoint-list` | 2.0 |
| `admin.endpoints.revoke` | admin | `admin.endpoints` | `endpoint-params` | `empty-result` | 2.0 |
| `admin.endpoints.set_enabled` | admin | `admin.endpoints` | `set-enabled-params` | `set-enabled-result` | 2.0 |
| `admin.endpoints.set_default` | admin | `admin.endpoints` | `set-default-params` | `empty-result` | 2.0 |
| `admin.shutdown` | admin | `admin.shutdown` | `shutdown-params` | `empty-result` | 2.0 |

`admin.endpoints.set_enabled(false)` revokes a live lease at once
(`endpoint.lease_changed`) and never auto-rebinds. The three mutating
admin methods are a **runtime overlay**: they change the running
daemon's view and are never written to `config.yaml`, so a restart
returns to the configured state; `admin.endpoints.list` says
`persisted: false` on every row (ADR-0028). Trust administration
(ADR-0032) arrives in 2.1 (A 2026-10-03, Stage 15's R2):

| Method | Domain | Capability | Params | Result | Since |
|---|---|---|---|---|---|
| `admin.trust.list` | admin | `admin.trust` | none | `trust-list` | 2.1 |
| `admin.trust.set` | admin | `admin.trust` | `trust-set-params` | `empty-result` | 2.1 |

`admin.trust.list` answers every peer the profile's `PeerTrustPolicy`
names with its `TrustDecision` and the policy's default; `admin.trust.set`
takes one `peer` and one `decision` (`trust-api`'s vocabulary, never a
free string). A set that revokes closes every connection the peer holds
at once, and every connection with `events` sees `peer.disconnected` with
`reason_class: policy` (below). The two are the same runtime overlay as
`admin.endpoints.*` — never written to `config.yaml`, `persisted: false`
— until the owner decides persistence (ADR-0028's question, routed with
the Stage 15 record). Their schemas, `trust-list` and `trust-set-params`,
and the method and capability enums' minor bumps land `approved` with
the implementing batch and its Rust mirror, as every 2.0 shape did
(plan §16 (3)), and flip `active` with Stage 15's close. Discovery and
bootstrap administration still have no method; they stay Stage 15's.

## Event catalogue

Every `event` frame's `event_type` binds its `data` to a shape
([`schemas/ipc/event.schema.json`](./schemas/ipc/event.schema.json)):

| Event type | Data | Delivered to | Since |
|---|---|---|---|
| `message.direct` | `endpoints:message-received` | exactly the connection holding the destination endpoint's lease | 2.0 |
| `message.broadcast` | `ipc:broadcast-received` | every connection with `events` holding a join reference for the channel | 2.0 |
| `endpoint.lease_changed` | `ipc:lease-changed` | the connection whose lease was revoked | 2.0 |
| `peer.disconnected` | `{peer, reason_class}` | every connection with `events` | 2.0 |
| `peer.path_changed` | `ipc:path-changed` (`peer`, `previous`, `current`, `reason_class`, `observed_at`) | every connection with `events` that has a route to the peer: a direct message exchanged with it, or a broadcast received from it on one of its joins | 2.1 |

A lease GRANT is learned from `hello_response`, not from an event;
`endpoint.lease_changed` carries revocation only: it is the IPC
projection of TRANSPORT.md's `EndpointLeaseChanged { state: registered |
released | revoked }` — a grant is learned from `hello_response` and a
release ends with the connection, so only `revoked` crosses the wire.
`peer.disconnected` is the runtime's `PeerDisconnected` (TRANSPORT.md
§Events) delivered to every connection holding `events`; its
`reason_class` is `policy` for a trust revocation (ADR-0012) — when a
trust change closed every connection the peer held — and `closed`
otherwise, the runtime's own name (#162); any further class is the
runtime's to name when it produces the event.
`peer.path_changed` (2.1, A 2026-10-03, Stage 15's R1) is the runtime's
`PeerPathChanged` (TRANSPORT.md §Events: `direct | relayed` either way,
with its `reason_class` and `observed_at`), the IPC projection of
LOCAL-CLIENT.md's session notice of the same name: delivered only to a
connection that has a route to the peer, coalesced per peer to the
latest pending (a replaced pending one is counted), in the ORDINARY
lane — it is droppable under §Push events item 1, unlike the four in
the reserved lane. Its schema `ipc:path-changed` lands `approved` with
the implementing batch and its mirror, as above.

## Version negotiation and phases

`hello.ipc_version.major` accepts any positive integer, so an unsupported
major is a well-formed hello: the server answers
`close{code: VersionIncompatible, supported: [{major: 2, minor: 0}]}` and
closes. For major 2 the server selects `minor = min(client, server)` and
returns it in `hello_response`. Minors are **additive only**: a new
method, event type or feature is emitted or accepted only when the
negotiated minor is at least the one that introduced it (the `Since`
columns above); adding, removing or changing a property of an existing closed shape is
a major once the first production build speaks 2.0; before it, an `approved`
schema takes an additive property into 2.0 itself (`event_queue` on
`hello_response`, A 2026-09-30), may remove a property no build has ever
emitted, its mirror refusing the old name (`pre_auth.tracked_peers` off
`admin-status` 1.1.0, A 2026-10-01), and treats a change as that removal
plus that addition — its own version moving 1.x → 1.(x+1) each time
(ADR-0017 records the rule and its one bound).
The first production build speaks 2.0.

Phases and directions, which JSON Schema cannot express and
`tests/ipc-v2` asserts: `hello` is the client's first frame and only its
first; `hello_response` and `close` are server-only; no `request`,
`cancel` or `pong` before `hello_response`; a `hello` not received within
**5 s** of the connection (a protocol constant, not a profile value) is
answered `close{Timeout}`.

## Close

`close` ([`schemas/ipc/close.schema.json`](./schemas/ipc/close.schema.json))
is the server's connection-fatal reply, sent — when the transport still
allows a write — before the connection is closed: for a handshake
refusal (with the handshake error codes above), for a framing or
protocol error before any request id exists (`ProtocolViolation`), for an
unsupported major (`VersionIncompatible`, with `supported`), for
keepalive expiry (`Timeout`), for shutdown (`ShuttingDown`), and for a
connection accepted past the connection limits of §Multiple clients
(`Overloaded`, before any hello is read; A 2026-09-29), and for a
request id reused while outstanding (`ProtocolViolation`). An error
that has a request id is a `response{ok: false}`, never a `close`.

## Cancellation mapping and request concurrency

A `cancel` for a request the server still holds pending — not yet handed
to the session — answers `CancelledBeforeDispatch` and the request is
dropped; a `cancel` for a request already handed over answers
`CancellationRaced` and the outcome, if any, is discarded. Each connection
has at most **16 requests in flight** (a protocol constant); further
requests wait in a pending queue of at most **48** (16 + 48 = the 64
outstanding commands per client of TRANSPORT.md §Backpressure); past
that bound the request is answered `Overloaded`.

`deadline_ms` is the caller's command deadline (TRANSPORT.md
§Cancellation): absent, the profile's command-deadline default;
present, clamped into 1..60 s, never refused — the range TRANSPORT.md
§send(destination, payload, options?) gives the direct-send command
deadline, adopted here for every IPC request.
A request whose deadline passes while pending is answered `Timeout`
and dropped; one whose deadline passes after hand-over is answered
`Timeout` and its later outcome discarded, as `CancellationRaced`
discards it — the port carries no deadline, so the server can stop
waiting, which is all the caller was promised, and cannot make the
binding stop. The server never lets a request outlive its deadline
silently (A 2026-09-29).

## Peer identity, lock and stale sockets (Unix)

Peer credentials are a **MUST** on Unix: the connecting peer's uid must
equal the owner uid of the runtime directory the daemon created; any
other uid is closed before `hello` and counted
(`ipc_peer_credential_refused_total`). This is the same-user boundary
ADR-0037 draws; a hostile same-uid process is SPIKE-005's, not v2.0's.

The daemon holds one exclusive lock, `<state>/profile.lock` (0600, an
advisory whole-file lock), for its lifetime; `transportctl identity
backup` and `restore` take the same lock, so they cannot run beside a
daemon. The lock file is released, never unlinked. A stale socket path
found at start is removed only by the lock holder, and only when it is a
socket owned by the daemon's uid; anything else at that path is fatal.
Shutdown order: stop accepting → revoke leases → settle bounded direct
responses → close the Swarm → unlink both sockets → release the lock.

## Platform scope of the v1 build

The first production build implements the Unix domain socket binding on
Linux. macOS is not decided: `ProfilePaths::resolve` fails without
`XDG_RUNTIME_DIR` by design and no macOS runtime-directory rule exists. The Windows named-pipe equivalent named throughout this
contract is the design, not the build: its ACL model and peer identity
are carried by name to the desktop-client stage.

## IPC v1 compatibility

There is no production v1 deployment requirement. The first production implementation targets IPC v2. If a future v1 adapter is added, it must be explicit and cannot reintroduce undocumented direct all-client fan-out into the v2 routing model.

## Connectivity status over IPC

There is no connectivity method. A data client holding `commands` receives the normalized `server_state.connectivity` push — direct/relay state and counts only — on connect and on change; the full backend-neutral `ConnectivitySummary` is `admin.status`'s, on the admin socket. Raw AutoNAT probe-server identities, relay PeerIds, relay multiaddrs, and server-capacity detail require a local diagnostics/admin capability and are never inferred as trust.

The runtime's `ConnectivityChanged` (TRANSPORT.md §Events) reaches IPC only as a `server_state` push, coalesced to at most one pending to avoid state-flap floods. It is not an `event` frame, not a durable replay stream, and does not change endpoint lease semantics.
