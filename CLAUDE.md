# Repository instructions

This is an architecture/specification repository.

## Hard boundaries

- Do not add production networking code, MCP server code, Rust crates, installers, or service units in the architecture phase.
- Broadcast means GossipSub. Directed traffic means a dedicated direct libp2p protocol. Do not route directed messages through GossipSub.
- Keep Claude Code isolated from libp2p concepts. Multiaddr, Swarm, ConnectionId, GossipSub mesh internals, Noise sessions, and Kademlia routing tables stop below the transport boundary.
- Discovery is replaceable and advisory. It does not dial, grant trust, manage pub/sub, or interpret payloads.
- Trust, discovery, connection management, broadcast, and direct messaging are separate responsibilities. v1 ordinary data-plane connections and outbound direct sends are deny-by-default trust-gated.
- GossipSub validation must follow ADR-0029: objective invalidity = `Reject`, valid-but-locally-unauthorized original publisher = `Ignore`, valid+authorized = `Accept`.
- A caller must be joined before broadcast; reply tokens do not recreate a left subscription.
- IPC v1 JSON bodies are capped at 128 KiB and must carry every legal 48 KiB transport payload after base64url/JSON expansion. Claude Channel IPC clients never receive administrative daemon-shutdown capability.
- No application semantics: no agent roles, Git workflow, task state, issue state, repository ownership, or project-management schema.
- Do not claim durable, ordered, exactly-once, or guaranteed delivery unless a future ADR changes the contract and an implementation actually provides it.
- Secrets and persistent PeerId keys are local state, never repository configuration.

When implementation begins, update ADR status rather than silently contradicting accepted decisions.
