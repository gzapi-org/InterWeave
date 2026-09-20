# ADR-0011 — amendment history

### Amendment 2026-09-20 — Discovery never writes the address book

Raised by p2p-network-dev while building the mDNS multicast mechanism
the owner ordered on 2026-09-20 (plan §14). `libp2p-mdns 0.49`'s
behaviour pushes `ToSwarm::NewExternalAddrOfPeer` for every discovered
(peer, address) pair, unconditionally (behaviour.rs:336-344); the Swarm
forwards it to every behaviour (libp2p-swarm 0.48 lib.rs:1165-1172). Any
host on the multicast domain could therefore place an address for any
PeerId into the Swarm. `providers/mdns.md` already says mDNS grants zero
trust and ADR-0012 says discovery never mutates trust, but nothing said
in terms that a provider may not write the address book — the
enforcement had never been needed, because no enabled behaviour consumes
the event today (kad, gossipsub, identify, autonat, relay, dcutr and the
vendored autonat: zero consumers, read at libp2p 0.57).

The decision makes the reading a rule so it binds the next provider and
the next library version: the emission is swallowed at the wrapper, the
only path from a discovered pair to a dialable address runs through the
provider's normalization, bounds and dedup into `DiscoveryManager` and
then ConnectionManager admission, and a multicast conformance test
asserts a discovered pair reaches the pipeline and nothing else
(`DISCOVERY-CONFORMANCE.md`, 2026-09-20). Nothing shipped at the time of
this amendment: `mdns` was enabled on p2p-network-dev's branch and the
behaviour not yet constructed.
