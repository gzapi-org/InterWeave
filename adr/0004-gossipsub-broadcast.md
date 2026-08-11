# GossipSub for one-to-many broadcast

**Status:** Accepted

## Context

The fundamental networking requirement is multi-peer broadcast and GossipSub directly matches topic-based one-to-many dissemination without a central broker.

## Decision

Map logical channels to domain-separated hashed GossipSub topics. Use signed messages and strict validation. GossipSub is the only v1 broadcast path and never substitutes for directed delivery.

## Alternatives considered

Floodsub; direct-send fanout; brokered pub/sub; custom flooding. Directed-over-GossipSub is explicitly excluded by requirements and is not reconsidered.

## Consequences

Best-effort mesh delivery and duplicate behavior must be documented. No offline delivery or global ordering. Mesh tuning becomes an implementation concern.

## Security implications

Signed source identity supports trust checks but forwarding peers can read payloads. Topic hashing reduces casual name leakage but not dictionary attacks.

## Operational implications

Mesh health and zero-peer topics need diagnostics. Discovery must independently supply/connect peers because GossipSub does not discover them.

## Implementation implications

Set conservative payload limits, normalized message IDs, strict validation, and runtime-level dedup/admission. Keep topic hash mapping deterministic/versioned.

## Revisit conditions

Revisit if interoperability/performance spikes show GossipSub cannot meet broadcast scale or if group E2EE changes topology requirements.
