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

A direct-capable session owns exactly one configured EndpointId lease. The runtime derives `source_endpoint` from that lease for every direct send/reply. No application API accepts a caller-supplied source endpoint.

The session may expose the neutral operations already defined by `TRANSPORT.md`: identity/status/connectivity, joins/leaves/subscriptions, broadcast, direct send/reply, peer diagnostics, and—when granted—remote endpoint directory queries.

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
LocalAdminPort   -> <profile>-admin.sock
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
8. no platform binding adds durable transport delivery.

These are shared conformance tests for desktop IPC and Android embedded-session adapters.
