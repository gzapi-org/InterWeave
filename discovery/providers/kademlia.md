# KademliaDiscovery (deferred v1)

## Decision

Architecture is specified, but the provider is not in the minimum v1 build. Wide-area discovery requirements must justify its attack surface and operational complexity first.

A minimum-v1 binary may recognize the provider type in configuration for forward compatibility, but `enabled: true` is a **hard configuration/startup error** until that binary actually includes an approved KademliaDiscovery implementation. `enabled: false` is permitted as a reserved disabled entry.

## Future algorithm

If implemented and enabled after an ADR update:

1. seed Kademlia with candidates learned through static/cache/other providers;
2. bootstrap the DHT only when at least one reachable DHT peer exists;
3. perform rate-limited `get_closest_peers` queries on random, namespace-independent keys to diversify the routing view;
4. normalize routing/query observations into CandidatePeer events;
5. use Identify/address observations where required by rust-libp2p to learn usable addresses;
6. expire stale provenance; never treat DHT presence as trust.

## Explicit exclusions

- no channel-name provider records;
- no channel membership records;
- no application role records;
- no DHT-backed authorization;
- no assumption that bootstrap nodes are honest.

## Threats

DHT poisoning, Sybil/eclipsing, stale addresses, privacy leakage, bootstrap capture, and network partitions. Mitigations include trust-gated data-plane connectivity/delivery, bounded query rates, peer diversity, multiple independent bootstrap hints, candidate caps, and future peer scoring/diversity policies.
