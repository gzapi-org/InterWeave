# Discovery options

Discovery answers only: **what PeerId may exist, at what candidate addresses, according to which source?**

| Provider | Scope | Strength | Primary weakness | v1 |
|---|---|---|---|---|
| Peer cache | local historical | fast restart/reconnect | stale/advisory | yes |
| mDNS | LAN | zero-config | multicast often blocked; untrusted LAN | optional yes |
| Static bootstrap | configured | deterministic entry points | operator maintenance / availability | yes |
| Kademlia | distributed | trust-bounded peer routing/address expansion in first integration | bootstrap, poisoning, privacy, convergence | standard-v1 supported; configured entries default enabled, opt-out |
| DNS | administrative | simple managed hints | central DNS/control plane | future |
| Rendezvous | namespace discovery | explicit peer-set rendezvous | rendezvous service dependency | future |
| HTTP directory | managed | enterprise policy integration | central service | future |
| Git hints | offline-friendly config | auditable | stale, not realtime | future |

## Composition

Providers run as independent tasks and feed normalized events into a `DiscoveryManager`. The manager merges candidates by PeerId, unions addresses, tracks provenance/expiry, and computes provider/aggregate health. It does not dial.

Priority is configuration metadata used by the ConnectionManager when equivalent candidates exist. It is not a hard-coded provider sequence. Startup may favor cache/static hints for immediate **authorized** dial attempts while mDNS runs concurrently; unauthorized candidates remain advisory observations. Discovery intensity can back off when sufficient trusted connectivity exists.

## Kademlia integration behavior

`KademliaDiscovery` has a complete standard-v1 design in `docs/architecture/kademlia-integration.md`; configured entries default enabled per ADR-0034. It uses a private custom Kademlia protocol namespace, explicit client/server mode, manual routing-table insertion, bounded bootstrap/targeted/random peer-routing queries, and no value/provider records. Random exploration keys are independent of ChannelId/application content. The first integration routes only through peers already authorized by `PeerTrustPolicy`, so it does not create discovery-only untrusted multiplexed connections.

ADR-0034 makes Kademlia part of the standard v1 implementation gate: configured entries default `enabled: true`, while explicit `enabled: false` remains an opt-out. SPIKE-003 and conformance/security evidence are required before release.
