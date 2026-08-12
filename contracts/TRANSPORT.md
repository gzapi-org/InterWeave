# Transport contract

Status: **architecture contract, v2 draft**. This contract is intentionally independent of Claude Code and libp2p.

Transport v2 incorporates network-addressable local endpoints from ADR-0030. The profile still owns one cryptographic transport identity; endpoints are routes inside that identity.

## Data types

```text
TransportIdentity = opaque stable string
PeerAddressHint   = opaque backend-neutral diagnostics only (not required by consumers)
EndpointId        = ASCII routing identifier, 1..64 bytes
EndpointAddress   = (TransportIdentity, EndpointId)
ChannelId         = ASCII identifier, 1..128 bytes
MessageId         = opaque exactly-128-bit identifier, printable form
Payload           = bytes + optional media_type
DirectDestination = { peer: TransportIdentity, endpoint: EndpointId? }
```

### EndpointId

Canonical grammar:

```regex
^[a-z][a-z0-9._-]{0,63}$
```

Endpoint IDs are case-sensitive lowercase ASCII route labels within one profile/PeerId. They are not human identity, application identity, role, trust, or a second cryptographic principal. Full semantics are in `contracts/ENDPOINTS.md`.

### ChannelId

Canonical grammar:

```regex
^[A-Za-z0-9][A-Za-z0-9._:/-]{0,127}$
```

Channel IDs are case-sensitive. No Unicode normalization is needed because v1/v2 are ASCII-only. The backend may hide raw names on the wire through a deterministic, domain-separated hash. A ChannelId has no application-defined semantics in this transport.

### MessageId

A v2 `MessageId` remains exactly **128 bits**. Backends may choose their printable representation at API boundaries, but the normalized value must round-trip without ambiguity. Increasing the identifier width is a transport/wire version change.

### Payload

- bytes are opaque to the transport;
- `media_type` is advisory; when present it is **1..128 ASCII bytes**; an empty string is non-canonical and rejected as `InvalidArgument` (use absence instead);
- hard ceiling for application payload bytes: **49,152 bytes (48 KiB)** for broadcast and direct operations;
- each active profile may configure a lower effective `max_payload_bytes`, never a higher one;
- the payload contract does not require UTF-8.

The Claude bridge may encode non-UTF-8 payloads for Channel `content`; that is a bridge representation concern, not a transport mutation.

## Capabilities

```text
TransportCapabilities {
  broadcast: true
  direct_delivery: true
  direct_endpoint_addressing: true
  endpoint_directory: true | false
  internet_reachability: true
  relayed_connectivity: true
  direct_path_upgrade: true
  durable_delivery: false
  offline_mailbox: false
  max_payload_bytes: <effective configured profile limit, <= 49152>
  max_channel_id_bytes: 128
  max_endpoint_id_bytes: 64
}
```

Consumers must branch on capabilities rather than infer backend behavior. `max_payload_bytes` reports the **effective configured limit for the active profile**, not merely the implementation ceiling.

## Lifecycle commands

### start(profile)

Starts or attaches to the configured transport runtime. Idempotent only when the same profile/configuration is already active. Failure categories: configuration, identity, local resource conflict, backend unavailable.

### shutdown(grace)

Stops accepting new work, drains in-flight control responses within a bounded grace interval, then terminates. It does not promise network delivery of queued messages.

`shutdown` is an administrative transport operation. A concrete IPC binding must not expose it to ordinary Channel clients; see `contracts/LOCAL-IPC.md`.

### local_identity()

Returns the stable transport identity for the active profile and an explicit `identity_epoch` that increments on deliberate rotation. It does not return a private key.

### health()

Returns aggregate health plus component summaries: `healthy | degraded | unavailable`. Health is operational, not an application workflow state.

### connectivity()

Returns a backend-neutral operational summary:

```text
ConnectivitySummary {
  direct_inbound: unknown | verified_public | not_verified,
  relay_inbound: unavailable | partial | ready,
  active_relay_reservations: u16,
  target_relay_reservations: u16,
  active_relayed_peer_paths: u16,
  hole_punch_inflight: u16,
  preferred_path_policy: direct_first,
  updated_at,
}
```

This exposes reachability state without exposing AutoNAT server identities, relay PeerIds, raw addresses, or backend protocol internals to ordinary consumers. Detailed infrastructure diagnostics remain local-admin data. Reachability state never grants trust.

## Local endpoint context

Direct-capable local clients operate under an exclusive configured EndpointId lease established by the IPC binding. The generic command dispatcher receives that EndpointId as trusted local caller context; callers do not provide an arbitrary `source_endpoint` argument.

A client without an endpoint lease may use diagnostics and broadcast operations if otherwise authorized, but direct `send` fails `EndpointNotRegistered`.

### local_endpoint()

Returns the caller's leased EndpointId and opaque 128-bit local lease epoch, or `none` for a non-direct diagnostics/admin client. A new epoch is generated for every granted lease, including after daemon restart; it is a stale-route discriminator, not an authorization secret or network identity.

### peer_endpoints(peer)

If the backend/profile advertises endpoint-directory capability, query a trusted peer for currently advertised endpoint routes. Results are advisory, bounded, and may be stale. A prior directory query is never required for explicit endpoint send.

Failure categories include `UnauthorizedPeer`, `PeerUnknown`, `PeerUnreachable`, `ProtocolUnsupported`, `Timeout`, and `Overloaded`.

## Subscription commands

### join(channel)

Acquire a local subscription reference to `channel` for the calling local client. Multiple local endpoints/clients may independently join. The daemon keeps the backend subscription while at least one local reference exists or while persistent configuration requires it.

Success means local subscription state was accepted; it does not prove remote peers exist.

### leave(channel)

Release the caller's subscription reference. Idempotent if the caller has no reference. It does not force other local clients to leave.

### subscriptions()

Returns the caller-visible channel set and aggregate local reference state without exposing backend mesh peers. The caller-visible set is distinct from profile-level `channels.desired`, which can keep backend subscriptions warm without granting any IPC client a join reference.

### Profile-desired subscriptions

`channels.desired` is a daemon/profile configuration mechanism, not an implicit local-client join. A desired channel may keep the backend GossipSub subscription/mesh warm while zero IPC clients are attached or joined. Inbound messages for such a channel are still validated and may participate in normal GossipSub propagation, but with no interested local IPC client they are dropped at local dispatch: **no buffering, replay, or future delivery is created**.

## Messaging commands

### broadcast(channel, payload, options?)

Publishes to all reachable trusted peers participating in the mapped broadcast topic.

The calling local client **must currently hold a join reference for `channel`**. v2 does not implicitly join, publish-and-subscribe, or borrow another local client's subscription. If the caller is not joined, return `ChannelNotJoined` before backend publication.

EndpointId is not inserted into the GossipSub envelope by the transport. Broadcast remains peer/channel scoped.

Success means the local backend accepted the publish. It does **not** guarantee any remote reception. Failure categories include invalid channel, `ChannelNotJoined`, oversized payload, overload, backend unavailable, and publish rejected.

### send(destination, payload, options?)

Sends one direct transport message using the backend's dedicated one-to-one protocol.

`destination` is:

```text
{ peer, endpoint? }
```

where omitted endpoint asks the receiver to route to its explicitly configured `default_direct_endpoint`. Omission never means local fan-out.

Default semantics:

- caller must own an active EndpointId lease; otherwise `EndpointNotRegistered`;
- source endpoint is derived from that lease and cannot be caller-spoofed;
- malformed/noncanonical explicit EndpointId input returns `InvalidArgument` before network work;
- endpoint outbound policy may narrow profile trust; a destination excluded by it returns `UnauthorizedPeer` locally before dialing;
- active `PeerTrustPolicy` applies to outbound remote direct destinations;
- `send({peer: local_identity(), ...}, ...)` returns `InvalidArgument`; self-dial is never attempted;
- an untrusted destination returns `UnauthorizedPeer` locally **before dialing**;
- connection reuse is allowed;
- for an authorized peer, runtime may dial using already-known direct or relay reachability paths; path selection is transparent to EndpointId/direct semantics;
- command deadline default: 10 s, configurable 1..60 s;
- success requires a remote **transport-accepted** response and returns the endpoint that actually accepted the message;
- a retry using the same message ID and destination selector is deduplicated against the first accepted route; a later default-endpoint change must not reroute that retry;
- no automatic application retry;
- no ordering guarantee across concurrent sends;
- no offline queue.

For an authorized target:

- `PeerUnknown`: no usable candidate reachability information is known for the target;
- `PeerUnreachable`: candidate information exists, but connection/protocol negotiation fails or misses deadline;
- `RemoteEndpointUnavailable`: remote direct v2 responds with coarse `no_route`; the local API does not claim whether the endpoint was unknown, offline, disabled, policy-denied, or default-unavailable.

A direct send never commands discovery providers to perform an ad hoc global search.

### peers()

Returns normalized peer diagnostics: identity, trust state, connection state, preferred path class (`direct | relayed | none`), last-observed time, and high-level discovery provenance names. Multiaddresses and relay infrastructure identities are omitted from the Claude default view but may be available to a local diagnostics CLI.

## Events

```text
PeerConnected { peer, path: direct | relayed, observed_at }
PeerDisconnected { peer, reason_class, observed_at }
ConnectivityChanged { summary: ConnectivitySummary }
MessageReceived {
  message_id,
  mode: broadcast | direct,
  source_peer,
  source_endpoint?,        // required for direct, absent for broadcast
  destination_endpoint?,   // resolved local endpoint for direct, absent for broadcast
  channel?,
  payload,
  received_at,
}
SubscriptionChanged { channel, state }
EndpointLeaseChanged { endpoint, state: registered | released | revoked, lease_epoch, at }
TrustPolicyChanged { revision, at }
TransportDegraded { component, reason_class, since }
TransportRecovered { component, at }
OverloadObserved { boundary, dropped_count_delta }
IdentityChanged { previous, current, identity_epoch }
```

A direct `MessageReceived` event is emitted only after transport authentication, profile trust admission, endpoint route/policy admission, framing/size validation, deduplication, and successful enqueue to exactly one owning local endpoint client.

A broadcast `MessageReceived` event remains join-filtered per local client.

`TrustPolicyChanged` is operational state only. Revoking a connected peer also closes its data-plane connection and produces `PeerDisconnected { reason_class: policy, ... }` where applicable.

## Error model

Stable categories, with backend detail hidden in diagnostics:

- `InvalidArgument`
- `PayloadTooLarge`
- `ChannelNotJoined`
- `EndpointNotRegistered`
- `EndpointUnknown`
- `EndpointInUse`
- `EndpointDisabled`
- `EndpointClientKindDenied`
- `CapabilityDenied`
- `UnauthorizedPeer`
- `PeerUnknown`
- `PeerUnreachable`
- `RemoteEndpointUnavailable`
- `Timeout`
- `Cancelled`
- `Overloaded`
- `BackendUnavailable`
- `ProtocolUnsupported`
- `VersionIncompatible`
- `ShuttingDown`
- `Internal`

Errors may include a non-sensitive diagnostic code and retry hint (`never | caller_may_retry | retry_after`).

## Cancellation

Every command has a request ID and cancellation token/deadline. Cancellation means the local caller no longer waits. It cannot retract a broadcast already published or a direct frame already accepted remotely. Implementations must surface this race honestly.

## Backpressure

No boundary is unbounded. Default architecture limits:

| Boundary | Default |
|---|---:|
| backend -> runtime normalized event queue | 1024 events |
| runtime -> each IPC client | 256 events |
| each client outstanding commands | 64 |
| concurrent direct sends per peer | 8 |
| concurrent direct sends total | 128 |
| configured endpoints/profile | 16 |
| advertised endpoints/profile | 16 |

When a local event queue is full, the runtime drops according to policy and increments diagnostics; it does not create a hidden durable queue. A direct inbound request whose target endpoint queue cannot accept the event is rejected as overloaded rather than acknowledged and lost.

## Delivery semantics

v2 intentionally provides:

- realtime, best-effort broadcast among locally admitted data-plane peers;
- direct transport acceptance into one resolved local endpoint queue, not application acknowledgement;
- local at-most-once presentation after bounded ephemeral deduplication;
- no global ordering;
- no durable network store;
- no offline mailbox;
- no exactly-once guarantee.

## Versioning

The **transport contract version** is a semantic major/minor pair. Endpoint addressing changes caller context, direct destination, and direct event semantics and therefore starts transport contract major **v2**.

Backend protocol versions and IPC versions are negotiated separately. Because no production v1 plugin/daemon exists, the implementation roadmap targets v2 directly rather than requiring a legacy fan-out compatibility layer.


See [`CONNECTIVITY.md`](./CONNECTIVITY.md) for normalized Internet reachability semantics.
