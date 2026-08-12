# Direct peer protocol

## Selected primitive

`libp2p::request_response` with a custom small codec.

Protocol ID:

```text
/claude-p2p-channel/direct/1.0.0
```

A future compatible minor may negotiate an additional protocol ID. Major incompatibility is explicit; do not silently reinterpret payloads.

## Request

Conceptual frame:

```text
DirectMessageV1 {
  message_id: 16 bytes,         // exactly 128 bits in transport v1
  sent_at_ms: u64,              // diagnostic only
  media_type_len: u8,
  media_type: bytes,
  payload_len: u32,
  payload: bytes <= effective profile max_payload_bytes <= 49152,
}
```

Codec must reject frames whose declared size exceeds limits before allocation. `sent_at_ms` is not an authorization, ordering, freshness, replay-window, or dedup input in v1.

## Response

```text
Accepted { message_id }
Rejected { message_id, reason_code }
```

Reason codes are coarse: unauthorized, overloaded, malformed, too_large, shutting_down, unsupported. Do not return sensitive policy details.

## Semantics

- one request-response exchange per direct message;
- a new substream may be opened per exchange while the underlying peer connection is reused;
- default total deadline: 10 seconds;
- sending to the local profile PeerId is invalid and fails locally as `InvalidArgument`; self-dial is never attempted;
- sender applies local PeerTrustPolicy before dialing; an unauthorized remote target fails locally as `UnauthorizedPeer`;
- receiver authenticates via libp2p connection PeerId, then applies PeerTrustPolicy and resource limits;
- `Accepted` means accepted into the receiver's bounded local event path, not processed by Claude;
- sender performs no automatic retry after timeout/connection failure;
- caller may retry with the **same message_id** if it wants deduplication to suppress duplicate local delivery;
- duplicate requests within TTL receive `Accepted`/duplicate-equivalent without re-emitting to local consumer;
- concurrent messages are unordered;
- one connection failure may fail all in-flight exchanges; each reports independently;
- cancellation only stops local waiting when the request is already in flight.

The normalized direct dedup key is:

```text
(mode=direct, source_peer, channel=None, message_id)
```

## Peer not connected

For an authorized target:

- if no usable candidate addresses are known, return `PeerUnknown` without triggering ad hoc discovery;
- if usable candidates exist, ConnectionManager may dial under the command deadline;
- if dialing/protocol negotiation fails or cannot complete within the deadline, return `PeerUnreachable`.

The direct operation does not command discovery providers to perform an ad hoc global search in v1.

## Graceful shutdown

Stop accepting new direct requests, respond `shutting_down` to newly arrived requests where possible, allow existing exchanges a short bounded grace, then close.
