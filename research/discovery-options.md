# Discovery options

Discovery answers only: **what PeerId may exist, at what candidate addresses, according to which source?**

| Provider | Scope | Strength | Primary weakness | v1 |
|---|---|---|---|---|
| Peer cache | local historical | fast restart/reconnect | stale/advisory | yes |
| mDNS | LAN | zero-config | multicast often blocked; untrusted LAN | optional yes |
| Static bootstrap | configured | deterministic entry points | operator maintenance / availability | yes |
| Kademlia | distributed | dynamic Internet-scale candidate graph | bootstrap, poisoning, privacy, convergence | no; designed for later |
| DNS | administrative | simple managed hints | central DNS/control plane | future |
| Rendezvous | namespace discovery | explicit peer-set rendezvous | rendezvous service dependency | future |
| HTTP directory | managed | enterprise policy integration | central service | future |
| Git hints | offline-friendly config | auditable | stale, not realtime | future |

## Composition

Providers run as independent tasks and feed normalized events into a `DiscoveryManager`. The manager merges candidates by PeerId, unions addresses, tracks provenance/expiry, and computes provider/aggregate health. It does not dial.

Priority is configuration metadata used by the ConnectionManager when equivalent candidates exist. It is not a hard-coded provider sequence. Startup may favor cache/static hints for immediate dial attempts while mDNS runs concurrently. Discovery intensity can back off when sufficient trusted connectivity exists.

## Kademlia future behavior

If enabled later, `KademliaDiscovery` must not advertise or query channel names as provider records. Its discovery job is network-level candidate expansion:

1. bootstrap the DHT using candidate peers supplied by other providers;
2. perform bounded `get_closest_peers` queries against random namespace-independent keys to refresh routing diversity;
3. normalize learned PeerIds/addresses into discovery events;
4. expire provenance according to observations and configured TTLs.

This avoids turning DHT provider records into channel membership or exposing raw channel names. A future ADR may revise this after empirical evaluation.
