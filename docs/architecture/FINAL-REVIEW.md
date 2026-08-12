# Final architecture review

Review posture: external CTO / implementation-readiness review. Original review 2026-08-11; amended through **Model B endpoint/human-client design on 2026-08-12**.

## Executive assessment

**Proceed to Phase 0 spikes and Phase 1 contract scaffolding with transport/config/IPC v2 as the implementation target.** The repository still contains architecture only.

The major evolution since the original prompt is deliberate and transport-generic: one profile PeerId can now host multiple explicitly addressable local EndpointIds so a human client, Claude bridge, and other applications can share identity/connections without duplicate direct delivery.

## Current boundary audit

| Review question | Result | Evidence / note |
|---|---|---|
| libp2p leaks into Claude/human client API? | **No** | clients use PeerId, EndpointId, ChannelId, payload, reply route over IPC |
| EndpointId becomes human/application identity? | **No** | route label only; explicit security/non-goal language |
| one PeerId can host human + Claude deterministically? | **Yes** | ADR-0030 + EndpointRegistry + IPC v2 exclusive leases |
| direct message can hit multiple local clients accidentally? | **No in v2** | exact endpoint/default resolution, one queue, no fan-out |
| peer-only direct behavior ambiguous? | **No** | explicit configured default endpoint or no_route |
| endpoint ACL can widen profile trust? | **No** | intersection invariant in contract/config/tests |
| source endpoint locally spoofable? | **No by API** | daemon derives source from IPC lease |
| remote source endpoint over-trusted? | **No by contract** | peer-asserted routing metadata only |
| direct Accepted overclaims application processing? | **No** | sent only after local endpoint queue admission, still transport-only |
| offline endpoint creates mailbox? | **No** | no_route; ADR-0020 persists |
| endpoint directory creates discovery/trust coupling? | **No** | separate trust-gated direct control protocol, not DiscoveryProvider |
| endpoint directory leaks all local apps? | **No by default** | active + advertise=true + trust/policy filtering + max32 |
| broadcast semantics changed by endpoints? | **No** | GossipSub/ChannelId/join refs unchanged |
| Kademlia stores endpoint presence? | **No** | records remain prohibited; endpoint directory is separate |
| Discovery grants trust? | **No** | unchanged deny-default trust boundary |
| ConnectionManager policy bypass? | **No** | root dial admission including Kademlia/direct/directory |
| GossipSub trust mapping explicit? | **Yes** | ADR-0029 Accept/Ignore/Reject |
| IPC can carry max payload + endpoint metadata? | **By contract/test requirement** | 128 KiB IPC v2 golden fixtures |
| daemon/Claude/human lifecycle coupled? | **No** | endpoint leases are runtime; PeerId survives client restart |
| human admin actions can be triggered by network payload automatically? | **No by architecture** | admin capability path separated from data plane |
| hidden persistent message state? | **No** | app-local human history is outside transport and only stores observed content |

## Confirmed current decisions

1. Official Claude Channel architecture remains the Claude integration model.
2. Separate profile-scoped daemon owns the network identity and Swarm.
3. rust-libp2p remains first backend behind neutral contracts.
4. GossipSub remains broadcast-only with ADR-0029 validation semantics.
5. Direct remains request-response, now endpoint-aware `/direct/2.0.0`.
6. One persistent PeerId belongs to a profile.
7. Model B adds configured `EndpointId` routes beneath that PeerId.
8. One direct-capable IPC v2 connection owns one exclusive endpoint lease.
9. Direct destination is `{peer, endpoint?}`; absent endpoint means receiver default, never fan-out.
10. Receiver sends AcceptedV2 only after exact endpoint queue admission.
11. Endpoint-specific policy can only narrow profile trust.
12. Direct dedup key uses source endpoint + wire destination selector + message ID; positive entries retain the first resolved route and content fingerprint, with an in-flight reservation closing concurrent retry races.
13. Direct reply binds exact remote source endpoint and local lease epoch.
14. Endpoint directory is optional, trust-gated, active/opt-in, identity-agnostic, and bounded.
15. Human client is another IPC v2 application endpoint with separately authorized administration (ADR-0032); it does not embed libp2p/private key.
16. Human contacts/display/history are application state above transport.
17. Broadcast remains per-client join state; desired channels are mesh pre-warm only.
18. No persistent offline network/endpoint/Claude/human delivery store exists.
19. Static PeerId trust still gates ordinary data-plane connections, direct peers, and source admission.
20. Noise remains per-link security; trusted GossipSub forwarders can see plaintext.
21. Kademlia remains fully designed optional peer-routing discovery but `enabled: false` by default and stores no app/channel/endpoint records.
22. IPC v2 remains owner-protected length-prefixed JSON with 128 KiB body and capability-scoped admin methods.

## Accepted limitations

- no network offline mailbox;
- no exactly-once/global order;
- EndpointId does not prove person/application identity;
- same-user malicious local process is partly inside IPC residual boundary;
- endpoint-directory advertisement leaks selected presence to trusted peers;
- endpoint directory can be stale;
- static trust does not scale to public networks;
- no group E2EE;
- no universal NAT traversal guarantee;
- Kademlia minimum build remains disabled/default-off;
- a human client can persist local history but cannot recover messages never accepted while it was offline.

## Remaining implementation risks / spikes

### Claude/MCP version skew

SPIKE-001 remains blocking before production bridge packaging.

### Direct v2 asynchronous acceptance

SPIKE-002 must verify request-response protocol-family negotiation/failure behavior and the practical pattern for withholding AcceptedV2 until bounded runtime endpoint queue admission. It may adjust task/channel mechanics, not endpoint routing semantics.

### Kademlia

SPIKE-003 remains required before optional Kademlia support is enabled.

### NAT/relay

SPIKE-004 determines real deployment requirements.

### Same-user local client authentication

SPIKE-005 remains conditional. Model B endpoint leases improve routing/isolation but do not cryptographically authenticate same-user client executables.

## No-production-implementation verification

Expected content remains Markdown/YAML architecture + Git metadata. No Cargo workspace, `.rs`, production MCP server, human client executable, daemon, installer, service unit, or identity key should exist.

## Implementation-readiness verdict

With ADR-0030/0031 and endpoint-aware contracts in place, a team can scaffold Phase 1 without reopening whether one PeerId can serve human + Claude, how direct traffic selects a local consumer, how replies return to the correct remote/local route, whether endpoint discovery implies identity/trust, or what happens while an endpoint is offline.
