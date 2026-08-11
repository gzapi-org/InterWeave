# Transport contract

Status: **architecture contract, v1 draft**. This contract is intentionally independent of Claude Code and libp2p.

## Data types

```text
TransportIdentity = opaque stable string
PeerAddressHint   = opaque backend-neutral diagnostics only (not required by consumers)
ChannelId         = ASCII identifier, 1..128 bytes
MessageId         = opaque 128-bit-or-stronger identifier, printable form
Payload           = bytes + optional media_type
```

### ChannelId

Canonical grammar:

```regex
^[A-Za-z0-9][A-Za-z0-9._:/-]{0,127}$
```

Channel IDs are case-sensitive. No Unicode normalization is needed because v1 is ASCII-only. The backend may hide raw names on the wire through a deterministic, domain-separated hash. A ChannelId has no application-defined semantics in this transport.

### Payload

- bytes are opaque to the transport;
- `media_type` is advisory and max 128 ASCII characters;
- v1 maximum application payload: **49,152 bytes (48 KiB)** for both broadcast and direct operations;
- the payload contract does not require UTF-8.

The Claude bridge may encode non-UTF-8 payloads for Channel `content`; that is a bridge representation concern, not a transport mutation.

## Capabilities

```text
TransportCapabilities {
  broadcast: true
  direct_delivery: true
  durable_delivery: false
  offline_mailbox: false
  max_payload_bytes: 49152
  max_channel_id_bytes: 128
}
```

Consumers must branch on capabilities rather than infer backend behavior.

## Lifecycle commands

### start(profile)

Starts or attaches to the configured transport runtime. Idempotent only when the same profile/configuration is already active. Failure categories: configuration, identity, local resource conflict, backend unavailable.

### shutdown(grace)

Stops accepting new work, drains in-flight control responses within a bounded grace interval, then terminates. It does not promise network delivery of queued messages.

### local_identity()

Returns the stable transport identity for the active profile and an explicit `identity_epoch` that increments on deliberate rotation. It does not return a private key.

### health()

Returns aggregate health plus component summaries: `healthy | degraded | unavailable`. Health is operational, not an application workflow state.

## Subscription commands

### join(channel)

Acquire a local subscription reference to `channel`. Multiple local clients may independently join. The daemon keeps the backend subscription while at least one local reference exists or while persistent configuration requires it.

Success means local subscription state was accepted; it does not prove remote peers exist.

### leave(channel)

Release the caller's subscription reference. Idempotent if the caller has no reference. It does not force other local clients to leave.

### subscriptions()

Returns the caller-visible channel set and aggregate local reference state without exposing backend mesh peers.

## Messaging commands

### broadcast(channel, payload, options?)

Publishes to all reachable peers participating in the mapped broadcast topic.

Success means the local backend accepted the publish. It does **not** guarantee any remote reception. Failure categories include invalid channel, oversized payload, not joined/policy (if configured), overload, backend unavailable, publish rejected.

### send(peer, payload, options?)

Sends one direct transport message to a specified `TransportIdentity` using the backend's dedicated 1:1 protocol.

Default semantics:

- connection reuse is allowed;
- runtime may dial using already-known candidate addresses;
- command deadline default: 10 s, configurable 1..60 s;
- success requires a remote **transport-accepted** response;
- no automatic application retry;
- no ordering guarantee across concurrent sends;
- no offline queue.

`NotConnected` is not immediately terminal when known addresses exist and dialing policy permits a dial. If no usable candidate exists, return `PeerUnreachable` without inventing discovery side effects.

### peers()

Returns normalized peer diagnostics: identity, trust state, connection state, last-observed time, and high-level discovery provenance names. Multiaddresses are omitted from the Claude default view but may be available to a local diagnostics CLI.

## Events

```text
PeerConnected { peer, observed_at }
PeerDisconnected { peer, reason_class, observed_at }
MessageReceived {
  message_id,
  mode: broadcast | direct,
  source_peer,
  channel?,
  payload,
  received_at,
}
SubscriptionChanged { channel, state }
TransportDegraded { component, reason_class, since }
TransportRecovered { component, at }
OverloadObserved { boundary, dropped_count_delta }
IdentityChanged { previous, current, identity_epoch }   // only explicit rotation/recovery
```

A `MessageReceived` event is emitted only after transport authentication, PeerTrustPolicy admission, framing/size validation, and local duplicate suppression.

## Error model

Stable categories, with backend detail hidden in diagnostics:

- `InvalidArgument`
- `PayloadTooLarge`
- `UnauthorizedPeer`
- `PeerUnknown`
- `PeerUnreachable`
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

When a local event queue is full, the runtime drops according to policy and increments diagnostics; it does not create a hidden durable queue. Control/health events may have a reserved small lane so overload remains observable.

## Delivery semantics

v1 intentionally provides:

- realtime, best-effort broadcast;
- direct transport acceptance response, not application acknowledgement;
- local at-most-once presentation after bounded ephemeral deduplication;
- no global ordering;
- no durable network store;
- no offline mailbox;
- no exactly-once guarantee.

## Versioning

The **transport contract version** is a semantic major/minor pair. Additive fields/capabilities may increment minor. Breaking command/event semantics increment major. Backend protocol versions and IPC versions are negotiated separately.
