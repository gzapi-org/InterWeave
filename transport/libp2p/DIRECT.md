# Direct peer protocol

## Selected primitive

`libp2p::request_response` with a custom bounded codec remains the selected primitive. ADR-0030 upgrades the initial implementation target to endpoint-aware protocol v2:

```text
/claude-p2p-channel/direct/2.0.0
```

The old architecture-only `/direct/1.0.0` frame is superseded before production implementation. Because there is no deployed v1 compatibility obligation, implementation should target v2 directly.

## Request

Conceptual frame:

```text
DirectMessageV2 {
  message_id: 16 bytes,          // exactly 128 bits
  sent_at_ms: u64,               // diagnostic only
  source_endpoint_len: u8,       // 1..64
  source_endpoint: bytes,
  destination_endpoint_len: u8,  // 0 => receiver default endpoint
  destination_endpoint: bytes,
  media_type_len: u8,
  media_type: bytes,
  payload_len: u32,
  payload: bytes <= effective profile max_payload_bytes <= 49152,
}
```

Endpoint strings must satisfy `EndpointId` grammar before routing. Codec rejects invalid/oversized declared lengths before allocation. `sent_at_ms` is not authorization, ordering, freshness, replay-window, or dedup input.

The local sender cannot choose `source_endpoint` arbitrarily: transport runtime obtains it from the calling IPC endpoint lease and passes it to the backend.

## Response

```text
AcceptedV2 {
  message_id,
  resolved_destination_endpoint
}

RejectedV2 {
  message_id,
  reason_code
}
```

Coarse reason codes: `no_route`, `unauthorized_peer`, `overloaded`, `malformed`, `too_large`, `shutting_down`, `unsupported`.

`no_route` deliberately collapses endpoint unknown, endpoint disabled, no active lease, missing default endpoint, and endpoint-specific policy denial.

## Semantics

- one request-response exchange per direct message;
- a new substream may be opened per exchange while the underlying peer connection is reused;
- default total deadline: 10 seconds;
- sending to the local profile PeerId is `InvalidArgument`; self-dial never occurs;
- sender applies endpoint outbound narrowing policy, then profile PeerTrustPolicy, before dialing;
- receiver authenticates PeerId, applies profile trust, validates frame, resolves endpoint/default route, applies endpoint inbound narrowing policy, checks active lease/queue capacity, and deduplicates;
- `AcceptedV2` is sent **only after** the resolved endpoint's bounded local event queue accepted the normalized direct event;
- `AcceptedV2` does not mean the human/Claude/application processed it;
- sender performs no automatic retry after timeout/connection failure;
- caller may retry with the same message ID;
- concurrent messages are unordered;
- one connection failure may fail all in-flight exchanges; each reports independently;
- cancellation only stops local waiting when already in flight.

The normalized direct dedup key is:

```text
(mode=direct, source_peer, source_endpoint, destination_selector[Explicit(id)|Default], message_id)
```

A positive duplicate entry stores the first `resolved_destination_endpoint` plus **DirectContentFingerprintV1** from `contracts/ENDPOINTS.md`: SHA-256 over the fixed domain-separated binary framing of media presence/length/value and payload length/value. Empty media type is invalid; timestamp and route fields are excluded. A retry with the same key and matching fingerprint returns `AcceptedV2` for that stored route without local re-delivery, even if the profile default subsequently changes. The same key with different content is rejected as malformed/duplicate conflict. `sent_at_ms` may differ on retry and is not part of the fingerprint.

Before route resolution, the runtime atomically acquires a bounded in-flight reservation for a cache miss. Only the owner executes route/queue admission. Matching concurrent duplicates attach as waiters and receive the same eventual response; content-fingerprint conflicts fail. Limits are 128 global / 8 per source PeerId by default, ceilings 512 / 32, aligned with direct in-flight admission; overflow is `overloaded` and never creates a parallel enqueue path. This is required to uphold local at-most-once presentation under concurrent retransmission, not only sequential retry.

## Peer not connected

For an authorized target:

- no usable candidate addresses -> `PeerUnknown` without ad hoc discovery;
- usable candidates -> ConnectionManager may dial under command deadline;
- connection/protocol negotiation failure -> `PeerUnreachable`;
- successful v2 exchange but remote `no_route` -> `RemoteEndpointUnavailable` locally.

The direct operation does not command discovery providers to perform an ad hoc global search.

## Endpoint omission

`destination_endpoint_len = 0` requests the receiver's explicit `default_direct_endpoint`. It never means all local clients. The `AcceptedV2` response reports the resolved endpoint so the caller can expose exact transport routing in diagnostics/result metadata.

## Graceful shutdown

Stop accepting new direct requests, respond `shutting_down` where possible, allow existing exchanges a short bounded grace, then close. Endpoint leases are revoked as part of daemon shutdown.

## Protocol family / future compatibility

A future compatible implementation may advertise multiple request-response protocol IDs where safe. Endpoint-addressed sends must never silently downgrade to a protocol that cannot preserve endpoint routing.
