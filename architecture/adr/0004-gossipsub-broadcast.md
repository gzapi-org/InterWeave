# GossipSub is the v1 broadcast primitive

**Status:** Accepted

## Context

The transport requires one-to-many realtime delivery to logical channels. Broadcast must be distinct from directed messaging, and the chosen mechanism must integrate with the initial rust-libp2p backend without defining application coordination semantics.

## Decision

Map logical channels to domain-separated hashed GossipSub topics. Use signed GossipSub messages with strict cryptographic/protocol validation. Enable explicit application validation reporting so authorization and protocol invalidity are mapped according to ADR-0029. Freeze the mesh-level GossipSub message-ID function to the full SHA-256 of a domain-separated canonical tuple containing the signed source PeerId raw bytes and the GossipSub wire sequence number, as specified in `transport/libp2p/PUBSUB.md`; application-envelope message ID is forbidden as the mesh duplicate key. GossipSub is the only v1 broadcast path and never substitutes for directed delivery.

## Alternatives considered

Custom flooding protocol; request-response fan-out; central broker; directed-over-GossipSub with recipient filtering.

## Consequences

Broadcast benefits from established mesh propagation and duplicate handling, but delivery remains realtime/best-effort and mesh behavior is topology-dependent. Local trust asymmetry can intentionally stop propagation of an otherwise well-formed message when the original publisher is not trusted locally; ADR-0029 makes that behavior explicit.

## Security implications

Signed source identity supports trust checks but forwarding peers can read payloads. Topic hashing reduces casual name leakage but not dictionary attacks. GossipSub validation results must not equate local authorization failure with objective wire invalidity. Source-and-sequence-bound mesh message identity prevents one trusted publisher from suppressing another publisher's message merely by racing/reusing the same application envelope ID, without requiring GossipSub to parse the application envelope.

## Operational implications

Mesh health, publish failures, validation outcomes, and zero-peer topics require diagnostics. A trusted data-plane overlay is required by ADR-0012; arbitrary discovered peers are not admitted merely to improve mesh size.

## Implementation implications

Set conservative payload limits, the frozen `GossipSubMessageIdV1` source+wire-sequence mesh identity, strict signature/protocol validation, explicit `Accept | Ignore | Reject` result reporting per ADR-0029, and runtime-level dedup/admission. Keep topic and message-ID mappings deterministic/versioned. Phase 2 fixtures must prove two signed publishers using the same 128-bit envelope ID remain distinct at the GossipSub cache and that invalid source/sequence signatures cannot pre-poison a valid duplicate-cache entry.

## Revisit conditions

Revisit if measured GossipSub behavior cannot satisfy target fan-out, latency, resource, or trust-topology requirements, without changing the generic broadcast contract unless necessary.
