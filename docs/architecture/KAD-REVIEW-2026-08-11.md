# Kademlia design amendment review — 2026-08-11

This memo is the review entry point for the Kademlia expansion requested after the contract-amendment review.

## Requested constraint

Keep Kademlia configured **`enabled: false`**, but fully design how it integrates into the project.

That constraint is preserved. Both Kademlia configuration examples use `enabled: false`, and the schema default is false. No production Kademlia code is present.

## What changed

### Normative decision

ADR-0009 now specifies Kademlia as a complete optional peer-routing design rather than a placeholder. It remains outside the minimum v1 provider set and default-disabled.

### Full integration blueprint

`docs/architecture/kademlia-integration.md` now defines:

- private protocol/network namespace and exact derivation;
- explicit client/server roles;
- first-generation trust/connection policy;
- Swarm-owned Kademlia driver vs DiscoveryProvider scheduling boundary;
- Identify/manual routing-table insertion;
- seeding and feedback-loop rules;
- bootstrap, targeted server-peer lookup, and random exploration;
- result normalization/TTL;
- record/provider-record prohibition;
- exact provisional resource/query defaults;
- health, failure, observability, reload, testing, and rollout criteria;
- Rust crate/module construction order.

### Configuration

`config/config.schema.yaml` contains the complete reserved Kademlia config shape. `config/examples/kademlia-ready-disabled.yaml` provides a reviewable full example while remaining disabled.

### Security boundary

The first Kademlia integration does **not** introduce untrusted discovery-only connections. Kademlia routing/query peers must already pass `PeerTrustPolicy`. This preserves the previously amended GossipSub confidentiality and connection-admission model.

### DHT scope

Kademlia is peer routing only:

- no ChannelId provider records;
- no membership records;
- no application values;
- no trust records;
- no application payload storage.

The design uses manual K-bucket insertion, disjoint query paths, record filtering, and bounded query/candidate resources.

### Coverage limitation

Because libp2p Kademlia client-mode nodes are not routing-table servers, the peer-routing-only design does not promise Kademlia lookup of arbitrary client nodes. Targeted PeerId lookup is only an opportunistic locator for server-mode DHT participants. Other discovery providers remain necessary for clients.

## Files most useful for review

1. `adr/0009-kademlia-role.md`
2. `docs/architecture/kademlia-integration.md`
3. `discovery/providers/kademlia.md`
4. `config/config.schema.yaml`
5. `config/examples/kademlia-ready-disabled.yaml`
6. `research/kademlia-integration.md`
7. `roadmap/SPIKES.md`
8. `roadmap/IMPLEMENTATION-PLAN.md`
9. `docs/architecture/testing.md`
10. `docs/architecture/threat-model.md`

## Explicit non-changes

- generic `DiscoveryProvider` contract is unchanged;
- generic `Transport` contract is unchanged;
- Kademlia does not grant trust;
- ConnectionManager owns connection admission/backoff/retention policy; a later review clarified that Kademlia behaviour can request Swarm dials, all of which must pass the root dial-admission gate;
- GossipSub/direct transport semantics are unchanged;
- bootstrap peers remain non-authoritative;
- `enabled: false` remains the default;
- no production Rust or network implementation was added.
