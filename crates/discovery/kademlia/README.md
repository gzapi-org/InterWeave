# kademlia

KademliaDiscovery provider using discovery-api + kademlia-control-api; libp2p-free provider logic.

**Current status:** Stage 10, active workspace member. Provider core: targeted-lookup eligibility (kademlia-integration.md §9.2, one conjunct at a time), query-result normalization with `"kademlia"` provenance and candidate TTL (§10), and health over the port's `RoutingView` (§14) — server mode reports at most degraded this stage, because the strong reachability evidence classes arrive with AutoNAT/Relay at Stage 11. Query budgets (§15: permit-before-invoke concurrency, a sliding rate window, and a charge for the library-started work F2 measured — taken from the driver's announcement of it and released by the handle that names that query, never by class) and §9.3 pacing/saturation are in. The Swarm-owned driver in `crates/transport/libp2p` lands later in this stage.

Kademlia is peer routing only (ADR-0009): candidates leave here as advisory observations, never as trust, membership, or application data. The crate touches no libp2p type and no socket; the composition root pumps `drain_commands`/`ingest_driver_event` between this provider and the driver.
