# libp2p

Concrete rust-libp2p Swarm backend: Noise, GossipSub, direct v2, endpoint directory, connection/dial admission, AutoNAT v2, Relay v2, DCUtR, Identify and Kademlia driver.

**Current status:** Stage 5, active workspace member. TCP, Noise, Yamux and Identify, plus the admission funnel Stage 5 requires before any autonomous behaviour exists: every outbound dial is reachable only through a ticket the root `ConnectionManager` issues, every inbound connection passes pre-Noise admission before its handshake begins, and every peer is classified from the profile's trust sources rather than assumed — including inbound, which is closed if the current policy does not retain it, and on revocation, which evicts the connections it withdraws.

GossipSub, direct v2, the endpoint directory, AutoNAT, Relay, DCUtR and Kademlia are absent from the feature list rather than merely unused, because a behaviour that is compiled in is one that can be switched on before its admission policy exists.
