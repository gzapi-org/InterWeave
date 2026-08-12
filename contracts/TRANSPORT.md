# Transport contract

Status: **architecture contract, v1 draft**. This contract is intentionally independent of Claude Code and libp2p.

## Data types

```text
TransportIdentity = opaque stable string
PeerAddressHint   = opaque backend-neutral diagnostics only (not required by consumers)
ChannelId         = ASCII identifier, 1..128 bytes
MessageId         = opaque exactly-128-bit identifier in v1, printable form
Payload           = bytes + optional media_type
```

### ChannelId

Canonical grammar:

```regex
^[A-Za-z0-9][A-Za-z0-9._:/-]{0,127}$
```

Channel IDs are case-sensitive. No Unicode normalization is needed because v1 is ASCII-only. The backend may hide raw names on the wire through a deterministic, domain-separated hash. A ChannelId has no application-defined semantics in this transport.

### MessageId

A v1 `MessageId` is exactly **128 bits**. Backends may choose their printable representation at API boundaries, but the normalized value must round-trip without ambiguity. Increasing the identifier width is a transport/wire version change, not a compatible v1 extension.

### Payload

- bytes are opaque to the transport;
- `media_type` is advisory and max 128 ASCII characters;
- v1 hard ceiling for application payload bytes: **49,152 bytes (48 KiB)** for both broadcast and direct operations;
- each active profile may configure a lower effective `max_payload_bytes`, never a higher one;
- the payload contract does not require UTF-8.

The Claude bridge may encode non-UTF-8 payloads for Channel `content`; that is a bridge representation concern, not a transport mutation.

## Capabilities

```text
TransportCapabilities {
  broadcast: true
  direct_delivery: true
  durable_delivery: false
  offline_mailbox: false
  max_payload_bytes: <effective configured profile limit, <= 49152>
  max_channel_id_bytes: 128
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

## Subscription commands

### join(channel)

Acquire a local subscription reference to `channel` for the calling local client. Multiple local clients may independently join. The daemon keeps the backend subscription while at least one local reference exists or while persistent configuration requires it.

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

The calling local client **must currently hold a join reference for `channel`**. v1 does not implicitly join, publish-and-subscribe, or borrow another local client's subscription. If the caller is not joined, return `ChannelNotJoined` before backend publication.

Success means the local backend accepted the publish. It does **not** guarantee any remote reception. Failure categories include invalid channel, `ChannelNotJoined`, oversized payload, overload, backend unavailable, and publish rejected.

### send(peer, payload, options?)

Sends one direct transport message to a specified `TransportIdentity` using the backend's dedicated 1:1 protocol.

Default semantics:

- v1 applies the active `PeerTrustPolicy` to outbound **remote** direct destinations; the local profile identity is not an external trust entry;
- `send(local_identity(), ...)` is invalid and returns `InvalidArgument` locally; libp2p self-dial is never attempted;
- an untrusted destination returns `UnauthorizedPeer` locally **before dialing**;
- connection reuse is allowed;
- for an authorized peer, runtime may dial using already-known candidate addresses;
- command deadline default: 10 s, configurable 1..60 s;
- success requires a remote **transport-accepted** response;
- no automatic application retry;
- no ordering guarantee across concurrent sends;
- no offline queue.

Error distinction for authorized targets:

- `PeerUnknown`: no usable candidate reachability information is known for the target, so no dial can be attempted without creating new discovery side effects;
- `PeerUnreachable`: usable candidate information exists, but connection establishment/protocol negotiation fails or cannot complete within the command deadline.

A direct send never commands discovery providers to perform an ad hoc global search in v1.

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
TrustPolicyChanged { revision, at }
TransportDegraded { component, reason_class, since }
TransportRecovered { component, at }
OverloadObserved { boundary, dropped_count_delta }
IdentityChanged { previous, current, identity_epoch }   // only explicit rotation/recovery
```

A `MessageReceived` event is emitted only after transport authentication, PeerTrustPolicy admission, framing/size validation, and local duplicate suppression.

`TrustPolicyChanged` is operational state only. Revoking a connected peer also causes its data-plane connection to be closed and produces `PeerDisconnected { reason_class: policy, ... }` where applicable.

## Error model

Stable categories, with backend detail hidden in diagnostics:

- `InvalidArgument`
- `PayloadTooLarge`
- `ChannelNotJoined`
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

- realtime, best-effort broadcast among locally admitted data-plane peers;
- direct transport acceptance response, not application acknowledgement;
- local at-most-once presentation after bounded ephemeral deduplication;
- no global ordering;
- no durable network store;
- no offline mailbox;
- no exactly-once guarantee.

## Versioning

The **transport contract version** is a semantic major/minor pair. Additive fields/capabilities may increment minor. Breaking command/event semantics increment major. Backend protocol versions and IPC versions are negotiated separately.
