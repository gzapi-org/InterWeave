# Integrate Kademlia as an optional peer-routing discovery provider, disabled by default

**Status:** Accepted

## Context

Kademlia can add distributed peer-routing and address discovery beyond peer cache, mDNS, and configured bootstrap hints. It also adds routing-table state, bootstrap/convergence behavior, behaviour-originated dial requests, privacy exposure, and poisoning/Sybil/eclipse risk. The project already requires that discovery remain advisory, that discovery never grant trust, and that ConnectionManager own connection policy.

The architecture previously deferred Kademlia from the minimum v1 build. This ADR keeps that rollout posture while fully specifying how Kademlia integrates when the optional implementation is added.

## Decision

Kademlia is a **fully designed but optional `DiscoveryProvider`**. It remains `enabled: false` by default and in shipped examples. A build that does not contain the approved Kademlia implementation MUST reject `enabled: true` as a hard configuration/startup error. A future build that contains the implementation still starts it only after an operator explicitly sets `enabled: true`.

The first Kademlia integration has these fixed semantics:

1. **Peer routing only.** Use Kademlia `FIND_NODE` / `get_n_closest_peers` and bootstrap behavior to learn peer identities and addresses. Do not use value records or provider records. Never store ChannelIds, channel membership, application roles, trust documents, or application payloads in the DHT.
2. **Private protocol namespace.** Do not join the public IPFS DHT. Derive a custom protocol name from the Kademlia wire major plus a non-secret lower-case ASCII deployment `network_id`, using the exact domain-separated SHA-256/base32 derivation frozen in `docs/architecture/kademlia-integration.md`.
3. **Explicit mode.** Default to Kademlia client mode. Server mode is an explicit operator choice for stable/reachable nodes; automatic promotion to server mode is not used in the first integration.
4. **Manual routing-table admission.** Configure `BucketInserts::Manual`. Feed eligible peer addresses into Kademlia explicitly after normal address validation, trust policy, and Identify/protocol-support observations. Identify is an authenticated protocol/address observation source, not authority.
5. **No untrusted discovery-only connections in the first integration.** A Kademlia routing/query peer must also be authorized by the active `PeerTrustPolicy` for ordinary data-plane connectivity. Kademlia does not create a hidden second connection-admission regime. In addition, every outbound Swarm dial requested by Kademlia's own iterative query engine is subject to ADR-0011's Swarm-wide `DialAdmissionGate`, so a peer returned by a DHT query cannot bypass trust/backoff/global limits merely because the dial originates inside `kad::Behaviour`.
6. **Connection policy remains outside the provider.** `KademliaDiscovery` schedules queries; the Swarm-owned driver executes Kademlia behavior. Kademlia may generate dial **requests**, but ConnectionManager remains the policy owner and the root dial gate may deny those requests for trust, per-peer backoff, shutdown, or resource-limit reasons.
7. **Capability-gated targeted lookup.** A targeted PeerId lookup is eligible only for an independently trusted peer with a **fresh advisory observation** that it advertised the exact project Kademlia server protocol/network namespace, and whose normal addresses are missing/unusable. That capability observation is learned from authenticated Identify and may persist in `PeerCacheDiscovery` across restart within the cache TTL. Client-mode nodes are not promised to be discoverable through peer routing alone.
8. **Bounded query/saturation strategy.** Seed from eligible candidates supplied by other discovery providers, bootstrap after at least one eligible DHT peer exists, and perform rate-limited random-key exploration while below an **effective routing target** bounded by configuration, `max_routing_peers`, and the current remote trust population. Repeated exploration rounds that yield no new trust-admitted routing peers back off and may mark a small overlay saturated instead of running every base interval forever.
9. **Namespace-independent lookup keys.** Random exploration keys are cryptographically random bytes and never derived from ChannelId, project names, message contents, or application identity. Targeted lookup keys are transport PeerIds of capability-observed server-mode DHT participants only; this is an opportunistic locator, not a directory for arbitrary client nodes.
10. **Ephemeral/advisory state.** Kademlia routing state is runtime state, not a durable membership database. Learned peers become normal `CandidatePeer` observations with Kademlia provenance and TTL. Peer cache may persist reachability and bounded protocol-capability observations, but neither grants trust or current liveness.
11. **Record APIs disabled by policy.** The implementation never calls `get_record`, `put_record`, `start_providing`, or `get_providers`. Configure incoming record filtering and discard/diagnose inbound record/provider-record writes rather than persisting them. Kademlia write-back caching is disabled.
12. **Security-oriented defaults and fail-safe configuration.** Use disjoint query paths, bounded parallelism, manual insertion, query/concurrency budgets, candidate/address caps, bootstrap diversity guidance, explicit client/server mode, cross-field validation, and enabled seed-source validation. Exact defaults are documented in `docs/architecture/kademlia-integration.md`.

## Alternatives considered

Mandatory Kademlia in minimum v1; joining the public IPFS DHT; DHT provider records keyed by ChannelId; DHT value records for membership/trust; open discovery-only connectivity to non-trusted DHT peers; Kademlia-generated dials exempt from ConnectionManager backoff; targeting any allowlisted peer without server-capability evidence; storing server role as authoritative membership; Kademlia as a trust/membership database; omitting Kademlia permanently.

## Consequences

The minimum v1 remains simpler and Kademlia remains opt-in. When implemented, it can expand reachability knowledge and locate trusted server-capable peers without changing the generic discovery/transport contracts. The first integration intentionally constrains the DHT topology to peers admitted by local trust policy; it is therefore a private/trust-bounded routing overlay rather than an open public peer directory.

The same PeerId may be learned from Kademlia and another provider; DiscoveryManager merges provenance normally. Kademlia does not bypass static bootstrap semantics or ConnectionManager policy. Behaviour-originated dials are an acknowledged backend execution path and must be measured/attributed in SPIKE-003.

Small trust sets no longer imply perpetual degraded health: the provider derives an effective target and can enter a saturated healthy state when bounded exploration stops discovering additional eligible routing peers.

## Security implications

Custom protocol names prevent accidental participation in unrelated public Kademlia networks but are not secrets. `network_id` is a namespace, not an authorization credential. Trust-gated routing and Swarm-wide dial admission prevent a malicious routing response from creating a successful connection to a locally unauthorized PeerId.

DHT poisoning, Sybil, eclipse, stale-address, bootstrap-capture, and traffic-analysis risks still exist inside the admitted routing set. Disjoint paths and bootstrap diversity reduce but do not eliminate them. A compromised trusted PeerId can still poison discovery observations.

Cached Kademlia server capability is advisory and freshness-bounded. It cannot grant trust or prove that a peer is currently a server/reachable.

## Operational implications

Operators need at least one reachable, trusted Kademlia server-mode seed to bootstrap a remote DHT. Client-mode nodes can query but do not serve as routing-table nodes under the selected model. Deployments therefore need an explicit server-role plan; bootstrap servers remain entry points, not authorities.

`enabled: false` produces no Kademlia queries and no Kademlia protocol participation. Enabling Kademlia on an unsupported build fails before transport startup. When enabled, every named `seed_source` must resolve to a configured enabled provider or configuration fails.

Server-mode reachability health is evidence-based but not AutoNAT-verified in this phase: explicit externally routable configured addresses and peer-observed addresses are diagnostics, not proof of inbound reachability.

## Implementation implications

The single libp2p Swarm task owns `libp2p::kad::Behaviour`. `KademliaDiscovery` owns scheduling, provider lifecycle, health, normalization, and budgets through the tiny neutral internal `kademlia-control-api`; it does not depend on `transport-libp2p` or own/poll the Swarm directly.

The future implementation must use the current rust-libp2p equivalent of custom protocol configuration, explicit client/server mode, `BucketInserts::Manual`, disabled caching/record storage, manual address insertion/removal, bootstrap and closest-peer queries, and query/routing events.

Identify observations are manually bridged into Kademlia address admission because rust-libp2p does not automatically couple Identify and Kademlia. Fresh protocol-support observations are also written through the existing peer-cache hint path for targeted-lookup eligibility after restart.

The root Swarm behavior includes an internal dial-admission mechanism driven by ConnectionManager state so Kademlia `ToSwarm::Dial` requests obey trust, backoff, and global limits. `KadCommand::Snapshot` has a defined bounded response for diagnostics/testing.

## Revisit conditions

Revisit this ADR if the project needs an open/public DHT, discovery-only connections to non-data-plane-trusted routers, Kademlia value/provider records, channel-scoped discovery records, automatic server-mode promotion, authoritative server-role configuration, or SPIKE-003 shows the proposed root dial admission/capability/saturation rules are not implementable with the selected rust-libp2p version.
