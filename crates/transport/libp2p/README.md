# libp2p

Concrete rust-libp2p Swarm backend: Noise, GossipSub, direct v2, endpoint directory, connection/dial admission, AutoNAT v2, Relay v2, DCUtR, Identify and Kademlia driver.

**Current status:** Stage 11, active workspace member. This paragraph said "Stage 5" from Stage 6 until now, which is the drift a status line is most prone to: nothing compiles it, and each stage that lands a behaviour is looking at the behaviour.

What is here: TCP, Noise, Yamux and Identify from Stage 4; direct v2 over request-response from Stage 6; signed GossipSub from Stage 7; the endpoint directory from Stage 8; the Kademlia driver from Stage 10. Under all of it sits the admission funnel Stage 5 required before any autonomous behaviour existed — every outbound dial reachable only through a ticket the root `ConnectionManager` issues, every inbound connection passing pre-Noise admission before its handshake begins, and every peer classified from the profile's trust sources rather than assumed, including inbound, which is closed if the current policy does not retain it, and on revocation, which evicts the connections it withdraws.

`autonat`, `relay` and `dcutr` are compiled as of Stage 11's features-on step; the behaviours themselves are not constructed yet.

Each of those was **absent from the `libp2p` feature list** rather than merely unused until the stage that earned it, because a behaviour that is compiled in is one that can be switched on before its admission policy exists. Stage 11 spent the last three entries, so the list withholds nothing now and the guarantee rests on the outbound gate, the trust classification and their tests. (The endpoint directory is not on that list at all — it rides on the `request-response` feature direct v2 already brought in, and is withheld by having no code rather than by the manifest.)
