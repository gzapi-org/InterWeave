# Final architecture review

Review posture: external CTO / implementation-readiness review. Original review date: 2026-08-11. **Amended after cross-document contract review on 2026-08-11.**

## Executive assessment

**Proceed to implementation spikes and Phase 1 contract scaffolding only with the amendments in this revision.** The prior architecture had three implementation-blocking inconsistencies: the 64 KiB IPC frame could not represent the 48 KiB payload after base64url/JSON expansion; trust admission did not clearly control outbound/direct and data-plane connection participation; and GossipSub validation results did not distinguish objective invalidity from local authorization failure.

Those issues are now resolved by ADR-0011/0012/0017/0026 amendments and new ADR-0029. No production implementation exists in this repository.

The strongest design property remains preserved: discovery is an independently replaceable advisory capability behind a stable interface. The Claude Channel bridge consumes only the generic transport/IPC contract and cannot control discovery internals. The selected libp2p/GossipSub stack is a backend, not the Claude-facing definition.

## Amendment closure audit

| Review issue | Resolution | Evidence |
|---|---|---|
| 48 KiB payload cannot fit 64 KiB IPC JSON | **Closed** | IPC JSON body is 128 KiB; bounded metadata + exact 49,152-byte golden fixtures are normative |
| untrusted connected peer could join/read GossipSub overlay | **Closed for v1** | unauthorized peers are not admitted/retained; explicit and behaviour-originated outbound dials pass trust admission; revocation evicts them |
| outbound `send` trust ambiguous | **Closed** | non-allowlisted target -> local `UnauthorizedPeer` before dial |
| GossipSub trust failure validation mapping absent | **Closed** | ADR-0029: objective invalid -> `Reject`; valid unauthorized original source -> `Ignore`; valid authorized -> `Accept` |
| `NotConnected` missing from error model | **Closed** | removed; `PeerUnknown` = no usable candidate, `PeerUnreachable` = candidate exists but dial/protocol cannot complete |
| dedup key drift | **Closed** | normalized key is `(mode, source_peer, channel_or_none, message_id)` |
| capability payload limit constant vs config | **Closed** | capabilities report effective configured value, hard ceiling 49,152 |
| trust-change event undefined | **Closed** | `TrustPolicyChanged { revision, at }`; revocation also yields policy disconnect |
| `media_type` / `content_type` drift | **Closed** | transport/libp2p = `media_type`; Claude-facing meta/tool = `content_type`; bridge mapping explicit |
| `sent_at` replay ambiguity | **Closed** | timestamps diagnostic only in both broadcast/direct v1 |
| MessageId width drift | **Closed** | exactly 128 bits in transport v1/direct frame |
| enabled unsupported Kademlia ambiguous | **Closed** | hard config/startup failure; disabled reserved entry allowed |
| publish without join ambiguous | **Closed** | calling client must hold join reference; otherwise `ChannelNotJoined` |
| IPC shutdown authority ambiguous | **Closed** | `admin.shutdown` capability required; `claude-channel` clients never granted it |
| static bootstrap DNS ownership ambiguous | **Closed** | provider emits unresolved DNS multiaddr; resolution failures are connection/dial diagnostics |
| broadcast reply token after leave ambiguous | **Closed** | reply fails `ChannelNotJoined`; token does not rejoin |
| discovery `confidence` overloaded | **Closed** | removed; provenance/freshness/provider priority remain explicit |

## Boundary audit

| Review question | Result | Evidence / note |
|---|---|---|
| libp2p leaks into Claude plugin API? | **No** | bridge tools/events use ChannelId, peer identity, payload, reply token only |
| discovery provider leaks into transport consumer? | **No** | only high-level provenance/health diagnostics cross runtime boundary |
| Kademlia mandatory? | **No** | fully designed optional peer-routing provider; default disabled; unsupported build rejects enablement |
| bootstrap becomes authority/trust? | **No** | static provider is reachability only and data-plane dial still requires allowlist |
| discovery becomes trust? | **No** | deny-by-default static trust remains independent |
| discovery manages connections? | **No** | ConnectionManager owns connection policy; backend protocol dial requests still pass the root admission gate |
| untrusted peers enter ordinary data-plane overlay? | **No in v1** | outbound/inbound retention is trust-gated |
| unbounded queues? | **No** | every named queue/client/concurrency pool has a bound |
| IPC can carry transport-max payload? | **Yes** | fixed 128 KiB JSON body + max-boundary fixtures |
| hidden persistent message state? | **No** | cache stores advisory reachability/protocol observations only; payload spool prohibited |
| incorrect delivery guarantees? | **No** | best effort/no offline/no exactly-once; direct acceptance precisely scoped |
| GossipSub local trust rejection mis-scored as invalid? | **No by contract** | ADR-0029 requires `Ignore` for valid unauthorized original publishers |
| trait over-abstraction? | **Controlled** | only Transport, DiscoveryProvider, TrustPolicy are public substitution boundaries |
| provider lifecycle underspecified? | **No** | stream/start/shutdown/health/cancellation/conformance defined |
| multi-instance host ambiguity? | **No** | identity per explicit profile; sharing only by selecting same profile |
| key ownership ambiguity? | **No** | profile daemon owns persistent key; bridge never sees it |
| Claude lifecycle coupled to network lifecycle? | **No** | daemon persists across bridge/session restart and bridge lacks shutdown capability |
| NAT complexity forced into v1? | **No** | conservative reachability, advanced NAT features deferred |
| broadcast uses GossipSub? | **Yes** | only broadcast backend |
| directed uses direct peer protocol? | **Yes** | request-response; GossipSub directed mode prohibited |
| official Telegram/Channel patterns reused? | **Yes** | stdio, capability, event, meta/content, gate, instructions, local-only trust mutation |
| Telegram-specific concepts leaked? | **No** | react/edit/chat IDs/polling stay out of generic API |
| bridge manages discovery? | **No** | only daemon status and generic operations |
| provider contract replaceable? | **Yes** | compile-time providers + config composition + conformance |
| daemon evolves independently? | **Yes** | versioned IPC/transport contract |
| restart changes PeerId? | **No** | persistent profile identity unless explicit rotation |
| discovery/trust/connection/pubsub/direct distinct? | **Yes** | separate ownership, with explicit policy crossings |

## Confirmed decisions

1. Current official Claude Channel architecture is the integration model.
2. Separate profile-scoped daemon is the network owner.
3. rust-libp2p is the first backend, isolated behind neutral contracts.
4. GossipSub is the broadcast primitive; validation uses explicit `Accept|Ignore|Reject` semantics.
5. request-response is the dedicated direct primitive with transport acceptance semantics.
6. v1 discovery is cache + optional mDNS + static bootstrap; Kademlia has a complete private peer-routing integration blueprint but remains optional/default-disabled and cannot be enabled in an unsupported build.
7. discovery candidates never grant trust or dial rights.
8. v1 trust is a static PeerId allowlist, deny by default, applied to ordinary data-plane connectivity, original message source, and outbound direct destination.
9. Noise protects peer connections; no group E2EE claim.
10. delivery is realtime/best effort, bounded, non-durable.
11. one persistent network identity belongs to a profile, and local sharing is explicit.
12. local IPC is owner-protected UDS/named pipe with versioned 128 KiB length-prefixed JSON and capability-scoped administration.
13. every legal 48 KiB transport payload must fit through IPC in either direction.
14. Claude tool surface is transport-only and contains no trust/key/discovery/shutdown administration.
15. broadcast requires a caller-owned join reference; reply tokens do not grant/recreate subscriptions.
16. profile `channels.desired` exists only for backend subscription/mesh pre-warm; with no joined client, inbound traffic is not buffered or replayed.
17. direct inbound messages to a shared profile fan out independently to every connected message-event IPC client; no hidden local primary exists.
18. `send` to the local PeerId is `InvalidArgument`; no self-dial occurs.
19. Kademlia behaviour-originated dials pass the same trust/backoff/global-limit admission policy as ordinary scheduler dials.
20. Kademlia targeted lookup requires fresh observed exact-server-protocol capability; small overlays use effective-target/saturation health.

## Accepted v1 limitations

- no persistent offline mailbox;
- no global order or exactly-once;
- no application identity/role binding to PeerId;
- no group E2EE from trusted forwarding peers;
- asymmetric trust lists can interrupt GossipSub propagation at nodes that `Ignore` an unauthorized original publisher;
- no universal NAT traversal guarantee;
- no Kademlia in minimum v1; the optional implementation is fully designed but remains default-disabled;
- static trust administration does not scale to large public networks;
- same-user malicious local processes remain partly inside the IPC residual threat boundary;
- channel/topic hashing does not defeat dictionary guessing;
- messages arriving while no local Channel client is connected may be dropped; profile-desired subscriptions deliberately do not create a hidden queue;
- same-profile direct messages may be presented to multiple local Claude bridges, each of which may reply.

## Remaining architectural risks

### Channel/MCP version skew

Claude Channels are version-sensitive. The bridge is isolated precisely to contain this risk. **SPIKE-001 remains blocking** before production bridge code.

### P2P Internet operations

The core architecture is sound for direct/LAN/configured paths, but real NAT/relay behavior must be proven against target deployment environments. Do not promise broad Internet reachability until SPIKE-004.

### Trust scalability and topology

Static PeerId allowlists are secure enough for controlled v1 networks but operationally poor at scale. Because local authorization also controls GossipSub propagation, inconsistent allowlists can reduce path diversity. Do not replace the model with TOFU/public discovery trust casually; design membership/revocation/channel-scoped authorization as a separate security project.

### GossipSub confidentiality

Any trusted forwarding peer can see plaintext payload. Deployments that cannot accept this limitation must encrypt payloads at a higher layer or wait for a separately designed group-security extension.

### IPC evolution

New metadata fields or larger diagnostics must preserve the 128 KiB max-payload fit invariant. If they cannot, the IPC format/ceiling must change deliberately rather than silently making legal transport payloads unrepresentable.

## Questions deferred to spikes

1. Exact current Claude `channels` manifest and MCP SDK compatibility? -> SPIKE-001.
2. Precise rust-libp2p request-response failure/cancellation behavior and codec ergonomics? -> SPIKE-002.
3. Do the proposed Kademlia defaults and trust-bounded topology materially improve target discovery without unacceptable poisoning/privacy cost, and can behaviour-originated dials/capability targeting/saturation be enforced exactly as specified? -> SPIKE-003 before optional implementation/support.
4. Which relay/NAT protocols are truly required for target deployments? -> SPIKE-004.
5. Is same-user local-process isolation needed beyond OS socket ACLs/capability scoping? -> SPIKE-005 if deployment requires.

GossipSub authorization-result mapping is **not** deferred to a spike; ADR-0029 fixes the v1 semantics. IPC max-payload representability is also a Phase 1 contract test, not an implementation judgment call.

## No-production-implementation verification

Expected repository content is Markdown/YAML architecture material plus Git metadata. There should be no `Cargo.toml`, `.rs`, production `server.ts`, executable daemon, MCP server, systemd unit, installer, or generated identity key. Final repository checks must enforce this before handoff.

## Implementation-readiness verdict

With these amendments, a Rust team can begin Phase 1 contract scaffolding without reopening IPC payload fit, trust/data-plane admission, GossipSub authorization mapping, shared-profile local fan-out, desired-subscription buffering semantics, Kademlia config invariants, or Kademlia connection-policy ownership. Kademlia remains implementation-gated by SPIKE-003 and disabled by default.
