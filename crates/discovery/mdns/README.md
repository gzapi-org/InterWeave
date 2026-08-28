# mdns

MdnsDiscovery implementation.

**Current status:** Stage 9, active workspace member. The normalization half of LAN discovery: raw `(peer, address)` observations pushed in by the backend become bounded, validated candidates. **This crate owns no socket** — the multicast mechanism is a libp2p behaviour in `crates/transport/libp2p`, because only that crate may own a Swarm.

Any host on the multicast domain can advertise, so mDNS grants **zero trust** and every bound is applied before an observation is emitted. A peer string outside the identity grammar is dropped, not repaired. A network that blocks multicast makes this provider *degraded*, never fatal, and leaves the other providers untouched.
