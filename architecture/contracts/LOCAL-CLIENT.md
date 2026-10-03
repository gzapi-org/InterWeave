# Local client session contract

Status: **Frozen architecture contract for first-party local clients**.

This contract defines the semantic boundary between a local application and `TransportRuntime` independently of whether that boundary is serialized over desktop IPC or implemented by an in-process Android adapter.

## 1. Why this contract exists

Model B requires the same invariants on desktop and Android:

- one profile-scoped PeerId;
- configured EndpointIds beneath that PeerId;
- exactly one live owner of a direct-capable EndpointId;
- source EndpointId derived from the local session rather than caller-controlled message fields;
- bounded queues and command concurrency;
- identical transport errors/events;
- administrative authority kept separate from message/event dispatch.

Desktop realizes this contract through IPC v2. Android realizes it inside the first-party app process. Neither realization changes network protocols.

## 2. Local data-plane session

A local data-plane session has immutable creation context:

```text
LocalDataSession {
  session_id: opaque 128-bit generation,
  client_kind: bounded local label,
  endpoint_lease: EndpointLease?,
  capabilities: bounded set,
  event_queue: bounded,
}
```

`session_id` is binding-local: it identifies the session to the process that opened it and never crosses the IPC wire; across bindings a grant is identified by its lease epoch (A 2026-09-30).

A direct-capable session owns exactly one configured EndpointId lease. The runtime derives `source_endpoint` from that lease for every direct send/reply. No application API accepts a caller-supplied source endpoint.

The session may expose the neutral operations already defined by `TRANSPORT.md`: identity/status/connectivity, joins/leaves/subscriptions, broadcast, direct send/reply, peer diagnostics, and—when granted—remote endpoint directory queries.

**Taking events (A 2026-10-03).** `events(max)` takes what waits for the session — session notices first, then direct messages, then broadcasts, each oldest first, at most `max` — and never waits. Beside it the port offers `ready()`: a future that resolves when at least one event is queued for the session or the session has ended, and otherwise waits. `events(max)` is unchanged by it and polling stays valid; `ready()` adds no delivery guarantee and changes no bound. A client that holds a session and never drains it still loses its lease under the binding's liveness rule (desktop keepalive): `ready()` is how a client wakes, not a licence to sleep. Each binding implements it with at most a per-session wake primitive beside its queue (the in-process binding drains on demand today and ipc-client holds a channel receiver that cannot wait without taking), and the shared conformance suite holds it (§7 item 9).

**Session notices (A 2026-10-03).** Beside `EndpointLeaseChanged` (revocation only; a grant is learned at open) and `PeerDisconnected` (`peer`, `reason_class`), the session's local notices are:

- `ServerState { health, connectivity? }` — the runtime's normalized health and `ConnectivitySummary` (direct/relay state and counts only, `TRANSPORT.md`'s `ConnectivityChanged`), delivered once at open and once per change, coalesced to at most one pending per session, in the lane that is never dropped for ordinary broadcasts. Over IPC it is the `server_state` frame (`LOCAL-IPC.md`); the in-process binding emits it from the runtime directly. A client learns connectivity from it and from nothing else on the data plane.
- `PeerPathChanged { peer, previous, current, reason_class, observed_at }` — `TRANSPORT.md` §Events' event, `direct | relayed` either way, delivered only to a session that has a route to the peer (a direct message exchanged with it, or a broadcast received from it on one of the session's joins), coalesced per peer to the latest pending with a replaced pending one counted, in the ordinary (droppable) lane. It carries no conversation and no message: a human client that already shows the peer changes nothing it displays but a route indicator (`human-client-ui.md` §13).

## 3. Lease semantics

Lease rules are identical for IPC and embedded adapters:

- configured-only registration;
- exclusive ownership;
- fresh 128-bit lease epoch for every grant;
- duplicate ownership returns `EndpointInUse`;
- disabled/unknown/kind-denied endpoints use the exact local error mapping from `ENDPOINTS.md`;
- session closure/revocation releases the lease immediately;
- no transport buffering is created while a route is unleased;
- remote traffic can never create/renew/transfer a local lease.

Desktop IPC additionally requires negotiated keepalive for endpoint leases by default. Android embedded sessions use Android service/process lifecycle signals instead of synthetic IPC keepalive; when the owning transport service stops, the session and lease are revoked synchronously.

## 4. Queue and acceptance semantics

Each local data-plane session has a bounded event queue. Direct `AcceptedV2` is withheld until the exact target session queue accepts the normalized event. Therefore a desktop IPC queue and an Android in-process queue are semantically interchangeable at the transport boundary.

A slow UI must not block the Swarm/runtime task. Queue overflow returns the existing coarse remote `overloaded` result and local `Overloaded` diagnostics; it never creates a hidden mailbox.

## 5. Administrative port

Administrative operations use a distinct `LocalAdminPort` abstraction. It is intentionally not a capability that can be obtained from `LocalDataSession`.

Desktop binding:

```text
LocalDataSession -> <profile>.sock
LocalAdminPort   -> <profile>.admin.sock
```

Android binding:

```text
LocalDataSession -> in-process mobile data-session adapter
LocalAdminPort   -> separate in-process admin facade reachable only from explicit local UI/control code
```

The Android distinction is a confused-deputy/software-architecture boundary, not protection against arbitrary code execution inside the same app process. Network/event handlers are constructed without an admin handle. Administrative actions require an explicit local user interaction path; sensitive identity/trust operations may additionally require Android user-presence policy.

## 6. Authority invariants

- `client_kind` never creates administrative authority.
- network payload handlers never receive an admin handle.
- administrative code cannot impersonate an EndpointId merely by selecting a route string; it must operate through normal configured session/lease creation.
- transport identity/recovery material never crosses the data-plane session contract.
- Android platform callbacks (network change, service lifecycle, notification taps) are platform events, not remote network authority.

## 7. Platform binding requirements

A platform binding must prove:

1. session-derived source endpoint;
2. exclusive lease ownership;
3. bounded event queue;
4. exact error/event mapping;
5. session teardown revokes the lease;
6. direct acceptance occurs after local queue admission;
7. data-plane callbacks cannot invoke administrative methods without a distinct local authority object;
8. no platform binding adds durable transport delivery;
9. `ready()` resolves when at least one event is queued or the session has ended, and `events(max)` after it takes what was there (A 2026-10-03);
10. a session sees one `ServerState` at open and one per change with never two pending, and a `PeerPathChanged` only for a peer it has a route to, the latest per peer (A 2026-10-03).

These are shared conformance tests for desktop IPC and Android embedded-session adapters. A first-party human application may persist content **after crossing this local-session boundary** only under ADR-0044; that application retention never changes queue admission, `AcceptedV2`, or transport durability semantics.
