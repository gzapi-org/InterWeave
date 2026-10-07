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
  local_peer: the profile's PeerId,
  client_kind: bounded local label,
  endpoint_lease: EndpointLease?,
  capabilities: bounded set,
  event_queue: bounded,
}
```

`session_id` is binding-local: it identifies the session to the process that opened it and never crosses the IPC wire; across bindings a grant is identified by its lease epoch (A 2026-09-30). `local_peer` is the profile's PeerId as the binding learned it at open — ipc-client from `hello_response`'s profile PeerId (`LOCAL-IPC.md` §Handshake), the in-process binding from the runtime's identity, a fake from its configuration — part of the creation context and no wire field of its own; it is the identity a data-plane client reports as its own (`plugin/TOOL-SURFACE.md` `local_peer_id`), since a data-plane session reaches no admin port to ask (A 2026-10-06, Stage 16 step 3).

A direct-capable session owns exactly one configured EndpointId lease. The runtime derives `source_endpoint` from that lease for every direct send/reply. No application API accepts a caller-supplied source endpoint.

The session may expose the neutral operations already defined by `TRANSPORT.md`: identity/status/connectivity, joins/leaves/subscriptions, broadcast, direct send/reply, peer diagnostics, and—when granted—remote endpoint directory queries.

**Taking events (A 2026-10-03).** `events(max)` takes what waits for the session — session notices first, then direct messages, then broadcasts, each oldest first, at most `max` — and never waits. Beside it the port offers `ready()`: a future that resolves when at least one event is queued for the session or the session has ended, and otherwise waits. `events(max)` is unchanged by it and polling stays valid; `ready()` adds no delivery guarantee and changes no bound. A client that holds a session and never drains it still loses its lease under the binding's liveness rule (desktop keepalive): `ready()` is how a client wakes, not a licence to sleep. Each binding implements it with at most a per-session wake primitive beside its queue (before Stage 15's R1 batch, #180, the in-process binding drained on demand and ipc-client held a channel receiver that could not wait without taking; the batch gave each a wake — the composition crate's notices, ipc-client's inbox), and the shared conformance suite holds it (§7 item 9). A bridge serving a host that speaks plain MCP exposes `events(max)` to that host as its `receive` tool through one bounded pull queue of its own, because it must keep draining this port whether or not the host calls; full, it pauses the drain and this port's bounds and the liveness rule decide, as for any client that stops draining — including the loss of what the pipeline held when the session ends as wedged (`CHANNEL-EVENT.md` §Delivery, ADR-0002 A 2026-10-07); this port is unchanged by it.

**Session notices (A 2026-10-03).** Beside `EndpointLeaseChanged` (revocation only; a grant is learned at open) and `PeerDisconnected` (`peer`, `reason_class`), the session's local notices are:

- `ServerState { health, connectivity? }` — the runtime's normalized health and `ConnectivitySummary` (direct/relay state and counts only, `TRANSPORT.md`'s `ConnectivityChanged`), delivered at open (or as soon as the runtime's state is first known) and whenever it changes, latest-state coalesced: at most one is pending per session, a later change before the drain replaces it, and the client sees the current state, not each change — the IPC binding samples the state, so a change reversed before the next sample is never pending — in the lane that is never dropped for ordinary broadcasts. Over IPC it is the `server_state` frame (`LOCAL-IPC.md`); the in-process binding emits it from the runtime directly. A client learns connectivity from it and from nothing else on the data plane.
- `PeerPathChanged { peer, previous, current, reason_class, observed_at }` — `TRANSPORT.md` §Events' event, `direct | relayed` either way, delivered only to a session that has a route to the peer — a direct message exchanged with it, or a broadcast received from it on one of the session's joins; a received message becomes a route at the moment the session TAKES it in `events()`, not when the runtime queues it (the runtime's delivery reports are lossy under backpressure while the message itself is not, so a route recorded at queue time could be lost silently), and a sent direct message at its acceptance; a path change between a message's admission and the session's next `events()` is therefore not owed (A 2026-10-04, Stage 15's R1 batch); a route ends only at a revocation that changed the policy (`admin.trust.set` to `false`): it ends every session's route to the peer and withdraws the peer's pending path notice, counted as a replacement, so after a re-allow nothing is owed until a new exchange makes a new route — a message from the peer queued before the revocation makes one when it is taken, and a send whose acceptance the session records after the revocation makes one — the take and acceptance rules are unchanged; a disconnect ends no route, but it withdraws the peer's pending path notice, counted as a replacement, so a notice taken after a `PeerDisconnected` never names the connection that is gone and always reports a change since the reconnect (A 2026-10-05, #190; the withdrawal A 2026-10-05 (ii), #192) — coalesced per peer — one pending per peer; a newer notice replaces it, keeping the pending one's `previous` and taking the newer `current` and `observed_at`, so a client never sees a `previous` it was not shown, and the replacement is counted; a merge whose `previous` equals its `current` announces no change and is withdrawn, counted as a replacement — in the ordinary lane, taken after every message within the room `max` leaves, so under pressure a pending notice waits and is never dropped — a route indicator stays stale until the notice is taken (the same rule LOCAL-IPC.md §Push events states for the IPC binding; A 2026-10-04 corrects A 2026-10-03's "dropped before any message", which no binding implements: one pending per routed peer under the ceiling below bounds the memory instead). A session is held to have a route to at most as many peers as the trust allowlist admits (`PeerTrustPolicy::MAX_ALLOWED_PEERS`, the composition's `MAX_ROUTED_PEERS`): a route past the bound is counted and not kept, so no notice is owed for that peer; pending path notices are held one per routed peer, apart from the message queue and its bound (A 2026-10-04). It carries no conversation and no message: a human client that already shows the peer changes nothing it displays but a route indicator (`human-client-ui.md` §13).

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

**Audit of trust sets (A 2026-10-04, Stage 15's R2).** Every trust set that reaches the runtime is recorded — the peer, the request, its outcome (changed, unchanged, refused, failed, and since ADR-0028 A 2026-10-07 unwritten — decided but not persisted because the overlay write failed, so nothing changed; one line, no path and no I/O error text) and the host's time — under the audit log target the composition names (`interweave::audit`; ADR-0012's consequence: "Trust changes can be reloaded locally and audited"). The record is the composition's, so it exists wherever the runtime is composed: the requirement binds every host, not the daemon alone. What a host owns is the sink — the daemon writes it to its log and admits the target at INFO whatever `observability.log_level` says (#186); an embedded host (Android, Stage 17) installs a sink of its own that admits the target the same way, since no filter it configures may silence it. A set refused before it reaches the runtime — for want of `admin.trust`, or by the IPC server before the port — changed nothing and is not recorded; who asked is not recorded either, because every administrative connection is the run-dir owner's (ADR-0037).

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
10. a session receives a `ServerState` at open or when the state is first known, never has two pending, and the one it drains is the current state; and a `PeerPathChanged` only for a peer it has a route to — a received message counting from the take, a sent one from its acceptance — the latest per peer, the route ending only at a revocation, a disconnect withdrawing the pending notice and keeping the route (A 2026-10-03; the moment A 2026-10-04; the end and the withdrawal A 2026-10-05).
11. trust administration is a `LocalAdminPort` capability of its own: without `admin.trust` the read and the set are `CapabilityDenied`; the read returns the local peer and the allowlist; allowing the local peer is `InvalidArgument`; allowing a listed peer succeeds; a revocation reaches an open session holding `events` as `PeerDisconnected { reason_class: policy }`, and the next read no longer lists the peer (A 2026-10-04, Stage 15's R2; `trust_administration_revokes_as_policy` asserts exactly this). Since ADR-0028 A 2026-10-07 a set is persisted at every production binding alike — the daemon and the embedded runtime each supply the composition's trust-overlay store at construction, under their own state directory — and every row of the read carries `persisted` and `source` (`configured | administered`) at every binding; the IPC mirror serialises them from minor 2.3 and serves the 2.1 row below it. A composition without a store is never served over IPC; a test that serves one keeps a store, as the daemon does (a 2.3 client would refuse unpersisted rows as `Internal`). The shared check asserts a revocation survives a restart of the runtime. The shared check also asserts (#190, c2f362c6) that the local peer is never among the allowed, that revoking an unlisted peer and allowing a listed one change nothing — the read-back compared as a set — and that the disconnect reaches every open session holding `events` (the composition registers exactly those for notices).

These are shared conformance tests for desktop IPC and Android embedded-session adapters. A first-party human application may persist content **after crossing this local-session boundary** only under ADR-0044; that application retention never changes queue admission, `AcceptedV2`, or transport durability semantics.
