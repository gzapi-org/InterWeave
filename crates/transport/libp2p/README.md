# libp2p

Concrete rust-libp2p Swarm backend: Noise, GossipSub, direct v2, endpoint directory, connection/dial admission, AutoNAT v2, Relay v2, DCUtR, Identify and Kademlia driver.

**Current status:** Stage 4, active workspace member. TCP, Noise, Yamux and Identify only — GossipSub, direct v2, the endpoint directory, AutoNAT, Relay, DCUtR and Kademlia are absent from the feature list rather than merely unused, because a behaviour that is compiled in is one that can be switched on before its admission policy exists.
