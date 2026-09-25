# MdnsDiscovery

Purpose: optional zero-configuration LAN candidate discovery.

## Behavior

- local-link multicast only;
- normalize discovered PeerIds and addresses;
- honor expiry events/record TTLs;
- tolerate duplicate discover/expire sequences;
- enforce per-peer/global bounds before emitting candidates.

## Security

Any host on the multicast domain can advertise candidates. mDNS therefore grants **zero trust**. PeerTrustPolicy remains required before ConnectionManager may dial/retain an ordinary v1 data-plane connection and before message source admission. LAN discovery also reveals that a P2P service exists; deployments with privacy requirements disable it.

**One store beneath this provider is unbounded, and enabling `mdns` admits it (Decision 2026-09-25, `DISCOVERY-CONFORMANCE.md`).** `libp2p-mdns 0.49.0` keeps every record it hears in an uncapped store, searched linearly, expiring on the announcer's own TTL; it reaches no dial and no book (§Address class), so a host on the multicast domain can spend this node's memory and CPU and nothing else. The mechanism ships gated off; an operator who enables it does so on a domain they control, knowing this. The store is bounded before the mDNS deadline reads MET (plan §14) — a vendored crate with a cap and eviction (ADR-0051's route) or a wrapper that replaces the inner behaviour at a cap; the mechanism, once chosen, is stated here — and a flood test measures the growth meanwhile.

## Address class (ADR-0052)

A discovered candidate is an address this runtime would dial because a peer supplied it, so it is inside ADR-0052's boundary (rule 1). mDNS's instance, stated here before the hook is written (rule 8):

**The floor stands, unchanged.** The candidate must be a literal IP multiaddr — no DNS name, no `p2p-circuit` component — and the special-use ranges ADR-0052 rule 2 lists are refused whoever supplies them. Any host on the multicast domain can say anything, so the floor has more work to do here than anywhere else, not less.

**Rule 3, private ranges: ADMITTED when this node itself holds a non-loopback listener in a private range of the same family** — the hole punch's condition (`DCUTR.md` §6) and for the same reason, reached from the other direction. mDNS is link-local multicast, so a peer that answered is on this LAN by construction; if this node holds no private listener of that family it has no LAN interface to reach that peer on, and a private candidate handed to it is the internal-network probe the floor exists to refuse. Without this clause LAN discovery yields nothing dialable on an ordinary RFC 1918 network, which is the case the provider exists for.

**Rule 4, source-equality: no clause, and the reason differs from the punch's.** A dial-back verifies the address a request arrived from, so it has an observed source to compare against. mDNS has no connection and no request: a candidate arrives in a multicast announcement from a host that need not be the peer it names. There is nothing to compare, which is why the floor and rule 3 carry the whole boundary here.

**What it costs, recorded rather than left to be discovered.** The floor refuses link-local, and an IPv6 network with no ULA and no global unicast — link-local only, which is a real LAN shape — therefore yields no dialable mDNS candidate. That is the floor working: an untrusted multicast announcement naming `fe80::` is indistinguishable from one probing this host's own interfaces. LAN discovery on such a network is unavailable, not degraded silently.

**Where it is enforced.** At the point the candidate is LEARNED, in the transport driver, before it reaches the provider: mDNS originates no dial, so there is no crate dial to deny and reissue (rule 5), and an address refused on class never becomes an observation at all. The learn-site refusal and the wrapper's swallow under ADR-0011 §Discovery never writes the address book are the same boundary seen from two sides — nothing the crate hears reaches either book except through this predicate and the pipeline — and, since PR #111 commit f85dd27, the crate's answer to the Swarm's pending-dial hook, the SECOND door a wrapped behaviour has, is empty: `libp2p-mdns 0.49.0` would otherwise append every multicast-named address to any dial that extends through the behaviours, past both the swallow and this predicate. The predicate is the sibling of `is_probeable_address` and `is_punchable_address` in the same module, under rule 6's subset test.

## Failure

Networks may block multicast, containers may lack multicast routing, and interfaces may change. Such failures make this provider degraded/unavailable but do not kill transport or static/cache discovery.
