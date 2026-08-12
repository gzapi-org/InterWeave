# Repository instructions

This is an architecture/specification repository.

## Hard boundaries

- Do not add production networking code, MCP server code, Rust crates, human-client executables, installers, or service units in the architecture phase.
- Broadcast means GossipSub. Directed traffic means the dedicated direct libp2p protocol. Do not route directed messages through GossipSub.
- One transport profile owns one persistent PeerId. Model B EndpointIds are local routes under that PeerId, not new identities.
- Direct-capable IPC v2 clients own one exclusive configured EndpointId lease. Direct v2 routes to exactly one endpoint; no undocumented all-client fan-out.
- A remote source EndpointId is peer-asserted routing metadata, not proof of a human, Claude instance, role, or authorization.
- Endpoint-specific policy may narrow but never widen profile PeerTrustPolicy.
- Endpoint directory is optional/trust-gated/opt-in/bounded and must never become DiscoveryProvider, GossipSub, or Kademlia state.
- Keep Claude Code and human UI isolated from libp2p concepts. Multiaddr, Swarm, ConnectionId, GossipSub mesh internals, Noise sessions, and Kademlia routing tables stop below transport/IPC boundaries.
- Discovery is replaceable and advisory. It does not dial, grant trust, manage pub/sub, route endpoints, or interpret payloads.
- Trust, discovery, connection management, endpoint routing, broadcast, and direct messaging are separate responsibilities.
- GossipSub validation follows ADR-0029: objective invalidity = Reject, valid-but-locally-unauthorized original publisher = Ignore, valid+authorized = Accept.
- Caller must be joined before broadcast; reply tokens do not recreate subscriptions.
- IPC v2 JSON bodies are capped at 128 KiB and must carry every legal 48 KiB payload plus max endpoint metadata. Data-plane clients never receive endpoint/shutdown admin capability.
- No application semantics: no human identity, social graph, agent roles, Git workflow, task state, issue state, repository ownership, read receipts, or project-management schema in transport contracts.
- No durable, ordered, exactly-once, guaranteed, or offline endpoint delivery claim unless a future ADR explicitly adds it.
- Human applications may persist messages they actually receive above transport; the daemon never uses that as an offline mailbox.
- Secrets and persistent PeerId keys are local state, never repository configuration.
- Standard v1 includes Kademlia support; configured Kademlia entries default `enabled: true` with explicit opt-out. No EndpointId/channel/application/trust records enter the DHT.

When implementation begins, update ADR status rather than silently contradicting accepted decisions.
