# Final architecture review

Review posture: external CTO / implementation-readiness review. Date: 2026-08-11.

## Executive assessment

**Proceed to implementation spikes and contract scaffolding.** The architecture satisfies the requested transport/plugin boundaries and makes the main limitations explicit. No production implementation exists in this repository.

The strongest design property is preserved: discovery is an independently replaceable advisory capability behind a stable interface. The Claude Channel bridge consumes only the generic transport/IPC contract and cannot control discovery internals. The selected libp2p/GossipSub stack is a backend, not the Claude-facing definition.

## Boundary audit

| Review question | Result | Evidence / note |
|---|---|---|
| libp2p leaks into Claude plugin API? | **No** | bridge tools/events use ChannelId, peer identity, payload, reply token only |
| discovery provider leaks into transport consumer? | **No** | only high-level provenance/health diagnostics cross runtime boundary |
| Kademlia mandatory? | **No** | explicitly deferred; provider-only future role |
| bootstrap becomes authority? | **No** | static provider is reachability hint only |
| discovery becomes trust? | **No** | deny-by-default static trust is independent |
| discovery manages connections? | **No** | ConnectionManager alone owns dial/reconnect decisions |
| unbounded queues? | **No** | every named queue/client/concurrency pool has a bound |
| hidden persistent message state? | **No** | cache is reachability only; payload spool prohibited |
| incorrect delivery guarantees? | **No** | best effort/no offline/no exactly-once; direct acceptance is precisely scoped |
| trait over-abstraction? | **Controlled** | only Transport, DiscoveryProvider, TrustPolicy are public substitution boundaries; Connection/PubSub remain modules |
| provider lifecycle underspecified? | **No** | stream/start/shutdown/health/cancellation/conformance defined |
| multi-instance host ambiguity? | **No** | identity per explicit profile; sharing only by selecting same profile |
| key ownership ambiguity? | **No** | profile daemon owns persistent key; bridge never sees it |
| Claude lifecycle coupled to network lifecycle? | **No** | daemon persists across bridge/session restart |
| NAT complexity forced into v1? | **No** | conservative reachability, advanced NAT features deferred |
| broadcast uses GossipSub? | **Yes** | only broadcast backend |
| directed uses direct peer protocol? | **Yes** | request-response; GossipSub directed mode is prohibited |
| official Telegram/Channel patterns reused? | **Yes** | stdio, capability, event, meta/content, gate, instructions, local-only trust mutation |
| Telegram-specific concepts leaked? | **No** | react/edit/chat IDs/polling stay out of generic API |
| bridge manages discovery? | **No** | only daemon status and generic operations |
| provider contract replaceable? | **Yes** | compile-time providers + config composition + conformance |
| daemon evolves independently? | **Yes** | versioned IPC/transport contract |
| restart changes PeerId? | **No** | persistent profile identity unless explicit rotation |
| discovery/trust/connection/pubsub/direct distinct? | **Yes** | separate ownership and ADRs |

## Confirmed decisions

1. Current official Claude Channel architecture is the integration model.
2. Separate profile-scoped daemon is the network owner.
3. rust-libp2p is the first backend, isolated behind neutral contracts.
4. GossipSub is the broadcast primitive; signed/strict validation is targeted.
5. request-response is the dedicated direct primitive with transport acceptance semantics.
6. v1 discovery is cache + optional mDNS + static bootstrap; Kademlia is deferred.
7. discovery candidates never grant trust or dial rights.
8. v1 trust is static PeerId allowlist, deny by default.
9. Noise protects peer connections; no group E2EE claim.
10. delivery is realtime/best effort, bounded, non-durable.
11. one persistent network identity belongs to a profile, and local sharing is explicit.
12. local IPC is owner-protected UDS/named pipe with versioned length-prefixed JSON.
13. Claude tool surface is transport-only and contains no trust/key/discovery administration.

## Accepted v1 limitations

- no persistent offline mailbox;
- no global order or exactly-once;
- no application identity/role binding to PeerId;
- no group E2EE from forwarding peers;
- no universal NAT traversal guarantee;
- no Kademlia in minimum v1;
- static trust administration does not scale to large public networks;
- same-user malicious local processes remain inside the IPC residual threat boundary;
- channel/topic hashing does not defeat dictionary guessing;
- messages arriving while no local Channel client is connected may be dropped.

## Remaining architectural risks

### Channel/MCP version skew

Claude Channels are still a version-sensitive/research-preview surface, while MCP itself changed significantly in 2026. The bridge is isolated precisely to contain this risk. **SPIKE-001 is blocking** before production bridge code.

### P2P Internet operations

The core architecture is sound for direct/LAN/configured paths, but real NAT/relay behavior must be proven against target deployment environments. Do not promise broad Internet reachability until SPIKE-004.

### Trust scalability

Static PeerId allowlists are secure enough for v1 controlled networks but operationally poor at scale. Do not replace them with TOFU/public discovery trust casually; design membership/revocation as a separate security project.

### GossipSub confidentiality

Any trusted forwarding peer can see plaintext payload. Deployments that cannot accept this limitation must encrypt payloads at a higher layer or wait for a separately designed group-security extension.

## Questions deferred to spikes

1. Exact current Claude `channels` manifest and MCP SDK compatibility? -> SPIKE-001.
2. Precise rust-libp2p request-response failure/cancellation behavior and codec ergonomics? -> SPIKE-002.
3. Does Kademlia materially improve target discovery without unacceptable poisoning/privacy cost? -> SPIKE-003.
4. Which relay/NAT protocols are truly required for target deployments? -> SPIKE-004.
5. Is same-user local-process isolation needed beyond OS socket ACLs? -> SPIKE-005 if deployment requires.

## No-production-implementation verification

Expected repository content is Markdown/YAML architecture material plus Git metadata. There should be no `Cargo.toml`, `.rs`, production `server.ts`, executable daemon, MCP server, systemd unit, installer, or generated identity key. Final repository checks must enforce this before handoff.

## Implementation-readiness verdict

A Rust team should be able to begin with contracts/spikes and spend implementation time on code, tests, interoperability, performance, and operations rather than reopening where discovery belongs, whether discovery grants trust, who dials, how broadcast/direct differ, who owns identity, how local processes communicate, or how multiple Claude instances share a host.
