# rust-libp2p as initial network backend

**Status:** Accepted

## Context

Requirements combine decentralized peer identity, encrypted direct connections, one-to-many pub/sub, multiple discovery mechanisms, and future NAT/relay support. libp2p already composes these concerns and has a mature Rust implementation.

## Decision

Select rust-libp2p as the first transport backend, using TCP, Noise, Yamux, GossipSub, request-response, Identify, and pluggable discovery integration. It remains an adapter behind the generic transport contract.

## Alternatives considered

Nostr; Matrix; NATS; Redis Pub/Sub; MQTT; custom WebSocket broker; a Telegram-style central service.

## Consequences

libp2p reduces custom wire protocol work but introduces nontrivial connectivity, peer-quality, and operational complexity. Backend isolation limits blast radius if a different backend is needed later.

## Security implications

libp2p supplies cryptographic transport identity, not policy authorization. Sybil/eclipse and discovery poisoning remain threats.

## Operational implications

Teams need peer/network diagnostics and connectivity tests rather than broker health alone. NAT behavior is an explicit deployment concern.

## Implementation implications

No libp2p dependency in transport API, discovery API, IPC contract, or Claude bridge crates. Pin versions only during implementation after a compatibility spike.

## Revisit conditions

Revisit if operational complexity outweighs decentralization benefits or another backend demonstrably satisfies both direct and broadcast semantics with lower cost.
