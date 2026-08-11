# rust-libp2p research

## Fit

rust-libp2p is appropriate as the first backend because one runtime can combine cryptographic peer identity, authenticated encrypted connections, multiplexed streams, pub/sub, several discovery mechanisms, and optional relay/NAT protocols while preserving backend modularity.

It is not the transport contract. The `transport-api` boundary is deliberately free of libp2p types.

## Direct messaging primitive

`libp2p::request_response` is selected over a bespoke stream handler for v1.

Research points:

- each logical request/response exchange opens a new substream on an existing multiplexed connection;
- the connection itself can be reused;
- callers receive explicit outbound/inbound failure events;
- a response channel can fail if the connection closes or a timeout occurs;
- custom codecs/protocol names allow a small binary or framed payload contract without defining a full custom `NetworkBehaviour` from scratch.

The architecture uses a request plus a tiny **transport-accepted** response. This avoids an ambiguous fire-and-forget write while not claiming application delivery. Automatic retries are deliberately outside the protocol primitive.

## GossipSub

GossipSub provides mesh-based topic dissemination and duplicate handling but **does not discover peers**. Discovery and connection management therefore remain separate.

The v1 blueprint selects signed GossipSub messages with strict cryptographic/protocol validation so a received pub/sub message can be associated with its publishing transport identity. Authorization is separate: v1 trust gates ordinary data-plane connections, and explicit GossipSub application validation maps objective invalidity to `Reject`, a valid but locally unauthorized original publisher to `Ignore`, and a valid authorized publisher to `Accept` (ADR-0029).

## Noise and multiplexing

The v1 backend uses TCP + Noise + Yamux. Noise authenticates the libp2p transport identity and encrypts each connection. The architecture does not treat connection security as group authorization or end-to-end group encryption.

## Discovery components

- mDNS: useful passive/zero-config LAN candidate discovery, optional.
- Kademlia: current rust-libp2p exposes custom protocol names, manual K-bucket insertion, explicit client/server mode, bootstrap, closest-peer queries, disjoint paths, and routing/query events. The project has a complete optional peer-routing design but keeps it disabled by default.
- Identify: useful after connection for protocol/address observations; not treated as discovery authority.
- static bootstrap and peer cache are project-defined `DiscoveryProvider` implementations, not libp2p swarm special cases.

## Reachability components

AutoNAT, Circuit Relay v2, DCUtR, and hole punching solve different reachability problems. v1 does not enable every mechanism by default. See ADR-0024 and `transport/libp2p/CONNECTIVITY.md`.


## Kademlia integration snapshot

The 2026-08-11 research pass inspected the `libp2p` 0.56.0 / `libp2p-kad` 0.48.0 documentation line. Key implementation facts and their project mapping are recorded in [kademlia-integration.md](kademlia-integration.md). Implementation must revalidate exact APIs before coding.
