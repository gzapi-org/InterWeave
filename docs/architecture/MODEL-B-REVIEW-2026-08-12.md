# Model B architecture review — 2026-08-12

Scope: complete design for a human client and other local applications sharing one profile PeerId through explicit network-addressable local endpoints.

## Decision summary

| Topic | Decision |
|---|---|
| network identity | one persistent PeerId per profile remains unchanged |
| local direct addressing | configured lowercase ASCII EndpointId, 1..64 bytes |
| local ownership | one exclusive endpoint lease per direct-capable IPC v2 connection |
| endpoint creation | configured-only; ordinary client cannot invent a route at handshake |
| direct wire | `/claude-p2p-channel/direct/2.0.0` with source + optional destination endpoint |
| omitted destination | receiver's explicit `default_direct_endpoint`, never fan-out |
| direct acceptance | only after resolved endpoint queue admission |
| endpoint ACL | can narrow profile PeerTrustPolicy, never widen |
| unavailable/denied route | coarse wire `no_route`; local `RemoteEndpointUnavailable` |
| dedup | key uses source endpoint + wire destination selector + message ID; positive entry stores first resolved endpoint and content fingerprint |
| reply | exact remote source endpoint + local endpoint lease epoch |
| endpoint directory | optional trust-gated `/endpoints/1.0.0`, active opt-in routes only |
| broadcast | unchanged; ChannelId/join-reference scoped, origin remains PeerId-only; per-endpoint authorship is application-layer |
| offline route | no daemon mailbox/buffer |
| human identity | application layer; not inferred from PeerId/EndpointId |
| human history | application-local persistence allowed after receipt/send; not transport durability |
| Kademlia | still default-disabled; never stores endpoint records |

## Superseded behavior

ADR-0016's previous architecture-only direct all-client fan-out is superseded by ADR-0030. No production v1 implementation exists, so the implementation roadmap targets transport/IPC/direct v2 without requiring a legacy fan-out compatibility layer.

## Boundary audit

- EndpointId does not become a cryptographic identity: **pass**.
- Endpoint policy cannot bypass profile trust: **pass**.
- Remote source endpoint cannot be treated as a verified human/application identity: **documented**.
- One remote direct message has exactly one local direct consumer: **pass**.
- Endpoint offline does not create hidden buffering: **pass**.
- Direct transport acceptance does not imply application processing: **pass**.
- Endpoint discovery does not leak through Kademlia/GossipSub: **pass**.
- Endpoint directory is opt-in/trust-gated/bounded: **pass**.
- Human admin controls remain separate from network-triggerable data plane: **pass**.
- Broadcast remains unaffected by endpoint routing: **pass**.
- IPC maximum payload invariant survives endpoint metadata: **required golden fixture**.
- Concurrent same-key direct retries cannot double-enqueue: **bounded in-flight reservation required**.
- Endpoint-directory enumeration is rate/concurrency bounded in addition to trust/advertisement filtering: **pass**.
- Human client data-plane/application/admin boundary is recorded normatively in ADR-0032: **pass**.

## Implementation blockers before Phase 1 freeze

No unresolved architectural blocker remains for Model B. Phase 1 must encode the endpoint config cross-field invariants, IPC v2 handshake/lease model, exact errors, and endpoint-aware dedup keys before networking code is written.

SPIKE-002 remains an implementation-detail validation for rust-libp2p request-response protocol-family behavior and asynchronous endpoint queue admission; it does not reopen the Model B routing decision.
