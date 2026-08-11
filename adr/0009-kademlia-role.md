
# Integrate Kademlia as an optional peer-routing discovery provider, disabled by default

**Status:** Accepted

## Context

Kademlia can add distributed peer-routing and address discovery beyond peer cache, mDNS, and configured bootstrap hints. It also adds routing-table state, bootstrap/convergence behavior, query traffic, privacy exposure, and poisoning/Sybil/eclipse risk. The project already requires that discovery remain advisory, that discovery never grant trust, and that ConnectionManager retain ownership of connection policy.

The architecture previously deferred Kademlia from the minimum v1 build. This ADR keeps that rollout posture while fully specifying how Kademlia integrates when the optional implementation is added.

## Decision

Kademlia is a **fully designed but optional `DiscoveryProvider`**. It remains `enabled: false` by default and in shipped examples. A build that does not contain the approved Kademlia implementation MUST reject `enabled: true` as a hard configuration/startup error. A future build that contains the implementation still starts it only after an operator explicitly sets `enabled: true`.

The first Kademlia integration has these fixed semantics:

1. **Peer routing only.** Use Kademlia `FIND_NODE` / `get_n_closest_peers` and bootstrap behavior to learn peer identities and addresses. Do not use value records or provider records. Never store ChannelIds, channel membership, application roles, trust documents, or application payloads in the DHT.
2. **Private protocol namespace.** Do not join the public IPFS DHT. Derive a custom protocol name from the Kademlia wire major plus a non-secret lower-case ASCII deployment `network_id`, using the exact domain-separated SHA-256/base32 derivation frozen in `docs/architecture/kademlia-integration.md`.
3. **Explicit mode.** Default to Kademlia client mode. Server mode is an explicit operator choice for stable/reachable nodes; automatic promotion to server mode is not used in the first integration.
4. **Manual routing-table admission.** Configure `BucketInserts::Manual`. Feed eligible peer addresses into Kademlia explicitly after normal address validation, trust policy, and Identify/protocol-support observations. Rust-libp2p Identify is treated as an address/protocol observation source, not as authority.
5. **No untrusted discovery-only connections in the first integration.** A Kademlia routing/query peer must also be authorized by the active `PeerTrustPolicy` for ordinary data-plane connectivity. Kademlia does not create a hidden second connection-admission regime. Designing protocol-scoped connections to untrusted DHT routers is a separate future ADR because multiplexed connections would otherwise reopen GossipSub confidentiality and protocol-admission questions.
6. **Bounded query strategy.** Seed from eligible candidates supplied by other discovery providers, bootstrap after at least one eligible DHT peer exists, perform rate-limited random-key `get_n_closest_peers` exploration when the trusted routing view is below target, and permit targeted lookup using a trusted **server-mode DHT participant** PeerId as the lookup key when that peer lacks usable addresses. Client-mode nodes are not promised to be discoverable through peer routing alone.
7. **Namespace-independent lookup keys.** Random exploration keys are cryptographically random bytes and never derived from ChannelId, project names, message contents, or application identity. Targeted lookup keys are transport PeerIds of server-mode DHT participants only; this is an opportunistic locator, not a directory for arbitrary client nodes.
8. **Ephemeral/advisory state.** Kademlia routing state is runtime state, not a durable membership database. Learned peers become normal `CandidatePeer` observations with Kademlia provenance and TTL; successful connection observations may separately flow to `PeerCacheDiscovery` through the existing cache hint path.
9. **Record APIs disabled by policy.** The implementation never calls `get_record`, `put_record`, `start_providing`, or `get_providers`. Configure incoming record filtering and discard/diagnose inbound record/provider-record writes rather than persisting them. Kademlia write-back caching is disabled.
10. **Security-oriented defaults.** Use disjoint query paths, bounded parallelism, manual insertion, query/concurrency budgets, candidate/address caps, bootstrap diversity guidance, and explicit client/server mode. Exact implementation defaults are documented in `docs/architecture/kademlia-integration.md` and remain configurable within bounded schema ranges.

## Alternatives considered

Mandatory Kademlia in minimum v1; joining the public IPFS DHT; DHT provider records keyed by ChannelId; DHT value records for membership/trust; open discovery-only connectivity to non-trusted DHT peers in the first implementation; Kademlia as a trust/membership database; omitting Kademlia permanently.

## Consequences

The minimum v1 remains simpler and Kademlia remains opt-in. When implemented, it can expand reachability knowledge and locate trusted peers without changing the generic discovery/transport contracts. The first integration intentionally constrains the DHT topology to peers admitted by local trust policy; it is therefore a private/trust-bounded routing overlay rather than an open public peer directory.

The same PeerId may be learned from Kademlia and another provider; DiscoveryManager merges provenance normally. Kademlia does not bypass static bootstrap semantics or ConnectionManager ownership.

## Security implications

Custom protocol names prevent accidental participation in unrelated public Kademlia networks but are not secrets. `network_id` is a namespace, not an authorization credential. Trust-gated routing peers prevent the first Kademlia implementation from reintroducing untrusted peers onto multiplexed connections that could otherwise interact with GossipSub/direct protocols.

DHT poisoning, Sybil, eclipse, stale-address, bootstrap-capture, and traffic-analysis risks still exist inside the admitted routing set. Disjoint paths and bootstrap diversity reduce but do not eliminate them. A compromised trusted PeerId can still poison discovery observations.

## Operational implications

Operators need at least one reachable, trusted Kademlia server-mode seed to bootstrap a remote DHT. Client-mode nodes can query but do not serve as routing-table nodes under the libp2p Kademlia client/server model. Deployments therefore need an explicit server-role plan; bootstrap servers remain entry points, not authorities.

`enabled: false` produces no Kademlia queries and no Kademlia protocol participation. Enabling Kademlia on an unsupported build fails before transport startup. Enabling it on a supported build with no eligible seeds degrades only that provider; other discovery providers and existing connections continue.

## Implementation implications

The single libp2p Swarm task owns `libp2p::kad::Behaviour`. `KademliaDiscovery` owns scheduling, provider lifecycle, health, normalization, and budgets through a narrow bounded control/event handle; it does not own or poll the Swarm directly. The future implementation must use the current rust-libp2p equivalent of:

- custom `kad::Config` protocol name;
- explicit `Mode::Client` / `Mode::Server`;
- `BucketInserts::Manual`;
- `Caching::Disabled`;
- incoming record filtering;
- `Behaviour::add_address` / `remove_address`;
- `Behaviour::bootstrap`;
- `Behaviour::get_n_closest_peers`;
- `Event::OutboundQueryProgressed`, `RoutingUpdated`, `RoutablePeer`, `UnroutablePeer`, and `ModeChanged`.

Identify observations must be manually bridged into Kademlia address admission because rust-libp2p does not automatically couple Identify and Kademlia.

## Revisit conditions

Revisit this ADR if the project needs an open/public DHT, discovery-only connections to non-data-plane-trusted routers, Kademlia value/provider records, channel-scoped discovery records, automatic server-mode promotion, or evidence from the implementation spike shows the bounded random-walk strategy cannot meet the required discovery objectives.
