# Libp2p endpoint routing and directory

This document specifies the libp2p realization of `contracts/ENDPOINTS.md`.

## Direct protocol v2

Endpoint-addressed direct traffic uses the existing request-response architecture with a new protocol ID:

```text
/claude-p2p-channel/direct/2.0.0
```

rust-libp2p request-response supports protocol families and per-protocol support, so protocol-version negotiation remains at the libp2p substream boundary rather than inside application payload bytes.

Conceptual request:

```text
DirectMessageV2 {
  message_id: 16 bytes,
  sent_at_ms: u64,                    // diagnostic only
  source_endpoint_len: u8,            // 1..64
  source_endpoint: ASCII bytes,
  destination_endpoint_len: u8,       // 0 => receiver default; otherwise 1..64
  destination_endpoint: ASCII bytes,
  media_type_len: u8,
  media_type: ASCII bytes,
  payload_len: u32,
  payload: bytes <= effective profile max_payload_bytes <= 49152,
}
```

`media_type_len = 0` encodes **absence**. No empty media-type string exists on the wire. A non-zero length encodes a present ASCII media type and maps to `media_present = 1`; zero maps to `media_present = 0` in `DirectContentFingerprintV1`.

The codec rejects invalid endpoint grammar, invalid lengths, or oversized declarations before allocating based on peer-controlled sizes.

Conceptual response:

```text
AcceptedV2 {
  message_id: 16 bytes,
  resolved_endpoint_len: u8,
  resolved_endpoint: ASCII bytes,
}

RejectedV2 {
  message_id: 16 bytes,
  reason_code: u8,
}
```

Wire rejection classes remain deliberately coarse:

- `no_route` — endpoint absent, disabled, offline, endpoint-policy denied, or default unavailable;
- `unauthorized_peer` — profile trust rejection where a structured response is safe/available;
- `overloaded`;
- `malformed`;
- `too_large`;
- `shutting_down`;
- `unsupported`.

Endpoint-specific denial does not produce an enumeration oracle.

## Runtime acceptance bridge

`direct_manager` must not send `AcceptedV2` merely because the request decoded. It sends an internal bounded admission request to `transport-runtime` and waits for one of:

```text
LocalRouteAccepted { resolved_endpoint }
LocalRouteRejected { local_reason_class }
```

`transport-runtime` owns endpoint configuration/policy/leases. `ipc-server` owns the socket connection but registers lease lifecycle into the runtime registry. This keeps libp2p codec code unaware of local process types.

## Endpoint directory protocol

Optional protocol:

```text
/claude-p2p-channel/endpoints/1.0.0
```

It is a separate request-response behavior/control protocol, not a GossipSub topic and not application payload.

Request:

```text
ListEndpointsV1 {}
```

Response:

```text
EndpointDirectoryV1 {
  generated_at_ms: u64,
  ttl_ms: u32,                     // <= 300000
  endpoints: [EndpointId; <= 32],
}
```

The list is sorted lexicographically for deterministic fixtures. Only currently leased endpoints configured with `advertise: true` are included. No endpoint metadata beyond `EndpointId` is carried.

The query itself is admitted only for a profile-trusted peer. Before including a particular endpoint, any endpoint inbound subset policy is applied to the querying peer; the directory never advertises an endpoint to a peer that would necessarily receive `no_route` from that endpoint's policy. Queries use a dedicated bounded budget: default **12 queries/minute/remote PeerId**, hard ceiling 60/minute, and default **16 concurrent directory exchanges/profile**, hard ceiling 64. Rate-limit/overload responses reveal no endpoint list.

## Directory cache

Remote endpoint-directory results may be cached **in memory only** for the response TTL (default 60 s). The cache is advisory UI/diagnostic state:

- it does not gate explicit endpoint send;
- it does not grant trust;
- it does not create an offline mailbox;
- stale cache entries are expected and a direct send may still return `no_route`;
- cache is discarded on daemon restart.

## Protocol compatibility

The implementation target is direct v2. `/direct/1.0.0` is not required because there is no production v1 deployment in this repository.

If a future compatibility adapter is added:

- an endpoint-addressed outbound request must never silently downgrade to v1;
- legacy inbound v1 routing policy must be explicit and disabled by default;
- legacy fan-out must not reappear as an undocumented endpoint-routing fallback.

## Connection behavior

Endpoint-directory lookup and direct sends use the existing trusted ConnectionManager path and global dial admission. They do not bypass trust, backoff, connection limits, or discovery ownership.

## Security properties

Noise authenticates the remote PeerId, not the claimed `source_endpoint`. `source_endpoint` is a route label under that authenticated peer's control. Applications must not treat it as proof of a person or role.

Endpoint enumeration is an information disclosure surface, which is why advertisement is opt-in, results are bounded, and queries are trust-gated.
