# Repository instructions

This is an architecture/specification repository.

## Hard boundaries

- Do not add production networking code, MCP server code, Rust crates, installers, or service units in the architecture phase.
- Broadcast means GossipSub. Directed traffic means a dedicated direct libp2p protocol. Do not route directed messages through GossipSub.
- Keep Claude Code isolated from libp2p concepts. Multiaddr, Swarm, ConnectionId, GossipSub mesh internals, Noise sessions, and Kademlia routing tables stop below the transport boundary.
- Discovery is replaceable and advisory. It does not dial, grant trust, manage pub/sub, or interpret payloads.
- Trust, discovery, connection management, broadcast, and direct messaging are separate responsibilities.
- No application semantics: no agent roles, Git workflow, task state, issue state, repository ownership, or project-management schema.
- Do not claim durable, ordered, exactly-once, or guaranteed delivery unless a future ADR changes the contract and an implementation actually provides it.
- Secrets and persistent PeerId keys are local state, never repository configuration.

When implementation begins, update ADR status rather than silently contradicting accepted decisions.
