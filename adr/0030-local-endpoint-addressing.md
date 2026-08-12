# Network-addressable local endpoints under one PeerId

**Status:** Accepted

## Context

ADR-0016 deliberately stopped at one PeerId per profile and documented that direct messages to a shared profile were duplicated to every event-capable local IPC client. That behavior is honest but unsuitable when a human client, Claude bridge, automation service, and other consumers intentionally share one network identity and require deterministic one-to-one routing.

The transport must preserve the distinction between network identity and local application routing. It must also keep remote messages from selecting arbitrary local processes by accident, preserve trust boundaries, and avoid creating a hidden mailbox when an endpoint is offline.

## Decision

Adopt Model B as transport contract v2:

- one profile owns one persistent PeerId;
- each direct-capable local IPC client claims one exclusive configured `EndpointId` lease;
- direct destination is `{peer, endpoint?}` where omitted endpoint means the receiver's configured default endpoint;
- direct protocol v2 carries both required `source_endpoint` and optional `destination_endpoint`;
- receiver resolves exactly one local endpoint and sends `Accepted` only after enqueue to that endpoint's bounded event queue;
- no direct local fan-out occurs in v2;
- endpoint-specific policy may narrow, never widen, profile PeerTrustPolicy;
- endpoint leases are runtime-only and disappear on IPC disconnect;
- endpoint identifiers are routing labels, not human/application identity proof or an authorization principal; endpoint ACLs remain PeerId-based;
- no daemon-side buffering exists for an unavailable endpoint;
- implementation target becomes `/claude-p2p-channel/direct/2.0.0` and IPC major version 2.

ADR-0016 is superseded for current direct-routing semantics; its profile identity and explicit-sharing decisions remain historical rationale.

## Alternatives considered

Keep v1 all-client fan-out; one PeerId per local application; first-connected or round-robin local consumer election; hidden daemon-assigned endpoint IDs; application-payload routing only; multiple PeerIds multiplexed inside one daemon.

## Consequences

A remote peer can address `PeerId P / EndpointId human` separately from `PeerId P / EndpointId claude`. Peer-only direct sends remain possible only through the remote profile's explicitly configured default endpoint. Deterministic replies return to the original source endpoint.

The transport contract, direct wire format, IPC handshake, message events, reply tokens, dedup key, configuration, testing, and human-client design all gain endpoint concepts.

## Security implications

Endpoint admission is subordinate to profile trust. Endpoint policy cannot authorize an untrusted PeerId. The sender cannot spoof a different local source endpoint because `TransportRuntime` derives it from the active local-session lease (IPC-bound on desktop, embedded on Android). A remote source endpoint remains peer-asserted metadata and is not cryptographic identity.

Endpoint claim conflicts fail closed. Same-user local compromise remains residual because the OS-user IPC boundary is not a full sandbox.

## Operational implications

Operators configure stable route names such as `human` and `claude`, choose at most one default direct endpoint, and may run multiple local clients without duplicate direct handling. An offline endpoint causes immediate remote `no_route`; it does not accumulate messages.

## Implementation implications

Add an `EndpointRegistry` in transport runtime, endpoint lease registration during IPC v2 handshake, direct protocol v2 codec fields, endpoint-aware deduplication, endpoint route admission before `Accepted`, endpoint status/observability, and endpoint-aware reply-token mapping.

No second Swarm or PeerId is created. Broadcast and discovery remain unchanged. Broadcast origin remains PeerId-only in transport v2: local EndpointId is deliberately not inserted into GossipSub envelopes, so applications needing per-endpoint broadcast authorship must define it above transport.

## Revisit conditions

Revisit if endpoint leases must be shared by multiple local processes, if cryptographic sub-identities are required, if offline endpoint delivery becomes a product requirement, or if endpoint-specific authentication must be stronger than the same-user IPC boundary.
