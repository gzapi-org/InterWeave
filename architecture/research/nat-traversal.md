# NAT traversal / reachability research snapshot

Research refreshed 2026-08-12 for the mandatory Phase 9 design. Primary upstream sources only. The docs.rs umbrella pages inspected for this pass report `libp2p 0.56.0`; SPIKE-004 must pin the exact implementation dependency rather than assuming `latest` remains equivalent.

## Selected upstream facts

### AutoNAT

Current rust-libp2p exposes both AutoNAT v1 and v2. AutoNAT v2 is split into client and server parts. Upstream documentation says v2 fixes false-positive and DoS issues from v1 by using a newly allocated dial-back port and asymmetric data cost. The client emits per-tested-address results including the test server PeerId.

Architecture consequence: target **AutoNAT v2** for new implementation and aggregate multiple per-server observations in our `ReachabilityManager`; do not treat one result as application trust or permanent reachability.

Sources:

- https://docs.rs/libp2p/latest/libp2p/autonat/v2/index.html
- https://docs.rs/libp2p/latest/libp2p/autonat/v2/client/struct.Event.html
- https://docs.rs/libp2p-autonat/latest/libp2p_autonat/v1/index.html

### Circuit Relay v2

rust-libp2p exposes a relay client behaviour/transport and relay server behaviour. Client events include reservation acceptance and inbound/outbound circuit establishment. Relay server configuration exposes reservation/circuit limits and per-peer/rate-limiter hooks.

Official libp2p documentation states relay connections are end-to-end encrypted, are not anonymous, and expose relay participation/PeerIds. Both endpoint peers know the path is relayed.

Architecture consequence: relay is a reachability path, never trust/identity authority; preserve end-peer Noise authentication and treat relay metadata visibility/availability as residual risk.

Sources:

- https://docs.rs/libp2p/latest/libp2p/relay/client/index.html
- https://docs.rs/libp2p/latest/libp2p/relay/client/enum.Event.html
- https://docs.rs/libp2p/latest/libp2p/relay/struct.Config.html
- https://docs.libp2p.io/concepts/circuit-relay/

### DCUtR

rust-libp2p provides `libp2p::dcutr::Behaviour`. Its event reports the remote PeerId and success/failure with a resulting direct ConnectionId. The upstream hole-punching tutorial composes Circuit Relay and DCUtR: the relay provides the working coordination path, then peers attempt a direct upgrade.

Architecture consequence: DCUtR failure is normal and must leave the relay path usable. A successful direct connection is a **new connection**; existing streams are not modeled as magically migrated.

Sources:

- https://docs.rs/libp2p/latest/libp2p/dcutr/struct.Behaviour.html
- https://docs.rs/libp2p/latest/libp2p/dcutr/struct.Event.html
- https://docs.rs/libp2p/latest/libp2p/tutorials/hole_punching/index.html

### Identify wiring

rust-libp2p documents that Identify is not implicitly wired to every other protocol; consumers must manually feed observed capabilities/addresses where needed.

Architecture consequence: the composition root explicitly routes Identify observations to the address registry, relay/AutoNAT candidate logic, Kademlia capability observation, and diagnostics.

Source:

- https://docs.rs/libp2p/latest/libp2p/identify/

### GossipSub isolation for infrastructure peers

Current rust-libp2p GossipSub exposes `blacklist_peer`; upstream source documents that messages are not sent to and are rejected from blacklisted peers.

Architecture consequence: infrastructure-only relay/probe connections can coexist in the Swarm while being excluded from the GossipSub data plane, subject to SPIKE-004 validating the complete protocol-admission matrix.

Source:

- https://docs.rs/libp2p/latest/libp2p/gossipsub/struct.Behaviour.html

## Design interpretation

The upstream protocols provide mechanisms, not this project's policy. The project therefore supplies:

- separate data-plane vs connectivity-infrastructure authorization;
- multiple-observer AutoNAT evidence aggregation;
- relay reservation redundancy/failover policy;
- direct-first path selection;
- DCUtR attempt/cooldown/retirement policy;
- explicit address provenance/expiry;
- common dial admission for behaviour-originated dials;
- bounded resources and observability.

No upstream source is interpreted as proving universal NAT traversal. Relay fallback is the availability mechanism when direct hole punching is impossible.
