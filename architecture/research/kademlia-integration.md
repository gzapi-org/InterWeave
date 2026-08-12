
# Kademlia integration research snapshot

Snapshot date: **2026-08-12**.

This note records primary-source facts used by ADR-0009 and `docs/architecture/kademlia-integration.md`. These are version-sensitive implementation facts and must be revalidated against the selected crate version before coding.

## Sources

- libp2p Kademlia DHT specification: https://github.com/libp2p/specs/blob/master/kad-dht/README.md
- rust-libp2p Kademlia crate/module: https://docs.rs/libp2p/latest/libp2p/kad/
- rust-libp2p Kademlia `Behaviour`: https://docs.rs/libp2p/latest/libp2p/kad/struct.Behaviour.html
- rust-libp2p Kademlia `Config`: https://docs.rs/libp2p/latest/libp2p/kad/struct.Config.html
- rust-libp2p Kademlia `Event`: https://docs.rs/libp2p/latest/libp2p/kad/enum.Event.html
- rust-libp2p `BucketInserts`: https://docs.rs/libp2p/latest/libp2p/kad/enum.BucketInserts.html
- rust-libp2p `StoreInserts`: https://docs.rs/libp2p/latest/libp2p/kad/enum.StoreInserts.html
- rust-libp2p `PeerInfo`: https://docs.rs/libp2p/latest/libp2p/kad/struct.PeerInfo.html
- rust-libp2p `NetworkBehaviour`: https://docs.rs/libp2p/latest/libp2p/swarm/trait.NetworkBehaviour.html
- rust-libp2p `ToSwarm`: https://docs.rs/libp2p/latest/libp2p/swarm/enum.ToSwarm.html
- rust-libp2p Identify `Info`: https://docs.rs/libp2p/latest/libp2p/identify/struct.Info.html

The docs snapshot inspected the `libp2p` 0.56.0 line / `libp2p-kad` 0.48.0 line.

## Facts used by the design

### Client/server distinction

The libp2p Kademlia specification distinguishes client and server nodes. Server nodes advertise/serve the Kademlia protocol; constrained or intermittently reachable nodes should operate as clients. The project therefore defaults to explicit client mode and requires operator opt-in for server mode.

### Custom protocol namespace

Current rust-libp2p `kad::Config::new` accepts a `StreamProtocol`. A custom protocol name segregates a private Kademlia network from unrelated DHTs. The project uses this to avoid accidental IPFS DHT participation and adds a deployment `network_id` namespace with a frozen domain-separated SHA-256/base32 derivation.

### Peer routing primitive

`Behaviour::get_closest_peers` and `get_n_closest_peers` initiate iterative closest-peer queries. Current results expose `PeerInfo` containing `peer_id` plus addresses. This is sufficient for the project's advisory CandidatePeer model without using DHT records.

### Bootstrap

`Behaviour::bootstrap` requires at least one known routing peer/address and performs a self lookup plus bucket refresh queries. Current rust-libp2p also has configurable periodic bootstrap behavior; the implementation must explicitly configure and observe it rather than rely on upstream defaults.

### Manual K-bucket insertion

`BucketInserts::Manual` means peers enter the Kademlia routing table only through explicit `Behaviour::add_address` calls. This is selected so provider observations cannot bypass project trust/address/resource policy.

### Identify is not automatically wired to Kademlia

Rust-libp2p documents Identify/Kademlia as decoupled: listen-address observations must be manually passed to `Behaviour::add_address`. The project therefore places this bridging responsibility in the libp2p backend/driver, after trust and address validation.

### Disjoint query paths

Current Kademlia config exposes `disjoint_query_paths`, documented as improving resilience in adversarial environments. The architecture enables it by default while still treating eclipse/Sybil resistance as incomplete.

### Records can be filtered

`StoreInserts::FilterBoth` causes inbound value/provider record insertions to be surfaced rather than automatically written to the record store. The project uses peer routing only and specifies that these writes are not persisted. Kademlia record lookup/write/provider APIs are not invoked by the provider.

### NetworkBehaviour can request Swarm dials

Rust-libp2p `NetworkBehaviour` controls which nodes a protocol tries to connect to, and `ToSwarm::Dial` instructs the Swarm to start a dial. Kademlia queries are iterative state machines that contact selected peers; therefore the architecture must not assume all network dials originate through the project's ordinary explicit dial scheduler. ADR-0011 resolves this with a root Swarm dial-admission policy that applies trust/backoff/limits to behaviour-originated dials too. SPIKE-003 must measure these dials rather than infer them from ConnectionManager scheduler calls.

### Identify protocol observations

Identify `Info` exposes the remote protocol list plus an `observed_addr`. The Kademlia targeted-lookup scheduler uses the exact advertised project Kademlia protocol as advisory evidence that a peer was operating as a server participant. Because this fact is unavailable before a first connection and may be needed after restart, the design persists a freshness-bounded protocol observation in PeerCacheDiscovery. It remains advisory and is superseded by fresh Identify evidence.

### Routing-table events

Current Kademlia events include query progress, routing updates, routable/unroutable peer observations, and mode changes. These provide enough data to drive provider health, diagnostics, and CandidatePeer normalization without exposing libp2p types beyond the backend.

## Design inference

The primary sources define protocol and library primitives, but they do not define this project's trust model. The decision to permit only data-plane-trusted peers as first-generation Kademlia routing/query peers is a project security decision derived from ADR-0011/0012 and the prior GossipSub confidentiality review, not a libp2p requirement.


## Peer-routing coverage limitation

The libp2p Kademlia specification says routing tables contain server-mode DHT peers; client-mode nodes can query the DHT but do not advertise/serve the Kademlia protocol and are not expected to populate routing tables. Therefore this project's peer-routing-only design can discover/reroute to server-mode DHT participants but must not claim to be a general locator for arbitrary client nodes. Discovering clients would require other providers or a separately designed rendezvous/provider-record mechanism.
