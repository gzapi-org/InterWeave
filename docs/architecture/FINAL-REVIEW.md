# Final architecture review

Review posture: external CTO / implementation-readiness review. Original review 2026-08-11; amended through **Model B Phase-1 freeze precision + identity-recovery design on 2026-08-12**.

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
| endpoint handshake/capability errors deterministic? | **Yes** | exact local error map in LOCAL-IPC/Phase-1 fixtures |
| direct retry race bounded/canonical? | **Yes on paper; spike required** | fixed DirectContentFingerprintV1 + 128/8 reservation limits + SPIKE-002 race test |
| recovery changes PeerId? | **No** | ADR-0033 encodes exact 32-byte Ed25519 secret and verifies expected PeerId |
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
21. Per ADR-0034, the standard v1 build includes Kademlia and configured entries default `enabled: true`; explicit opt-out remains supported, and Kademlia stores no app/channel/endpoint records.
22. IPC v2 remains owner-protected length-prefixed JSON with 128 KiB body and capability-scoped admin methods; version is negotiated, human data/admin sessions count separately, and optional keepalive can release wedged leases.
23. Claude Channel is not granted `endpoints.query` by default; `peer_endpoints` is explicitly deferred pending a security/tool-surface revisit.
24. DirectContentFingerprintV1 is fixed byte-for-byte and direct in-flight reservation state is capped at 128 global / 8 per source peer by default.
25. Initial software identity is Ed25519 with optional offline 24-word exact-key recovery (ADR-0033); mnemonic material never crosses IPC. Verify-only drills are read-only, and full profile disaster recovery also needs a separate config.yaml backup.
26. EndpointId leases require negotiated IPC keepalive by default; an explicit compatibility policy may relax this without changing lease ownership semantics.

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
- default-on Kademlia increases ordinary metadata/topology/privacy exposure and therefore makes SPIKE-003/conformance/security a standard-v1 release gate;
- a human client can persist local history but cannot recover messages never accepted while it was offline;
- the BIP-39-derived recovery UX has only an 8-bit mnemonic checksum, so expected-PeerId backup metadata is the stronger restore check;
- recovery phrase theft is full PeerId private-key compromise.

## Remaining implementation risks / spikes

### Claude/MCP version skew

SPIKE-001 remains blocking before production bridge packaging.

### Direct v2 asynchronous acceptance

SPIKE-002 must verify request-response protocol-family negotiation/failure behavior, the practical pattern for withholding AcceptedV2 until bounded runtime endpoint queue admission, and concurrent same-key retransmission against the real request-response scheduler so the in-flight reservation guarantee is empirically validated. It may adjust task/channel mechanics, not endpoint routing/dedup semantics.

### Kademlia

SPIKE-003 is required before the standard v1 build ships configured Kademlia entries default-enabled. Failure blocks/revisits ADR-0034 rather than silently shipping an unsupported default.

### NAT/relay

SPIKE-004 determines real deployment requirements.

### Same-user local client authentication

SPIKE-005 remains conditional. Model B endpoint leases improve routing/isolation but do not cryptographically authenticate same-user client executables.

### Identity-recovery portability

SPIKE-006 must verify that the pinned rust-libp2p Ed25519 identity API/portable serialization boundary round-trips the exact 32-byte secret assumed by `cp2p-ed25519-bip39-entropy-v1` and reproduces the same PeerId. Failure keeps production mnemonic backup/restore disabled; it does not authorize silently changing the recovery format.

## No-production-implementation verification

Expected content remains Markdown/YAML architecture + Git metadata. No Cargo workspace, `.rs`, production MCP server, human client executable, daemon, installer, service unit, or identity key should exist.

## Implementation-readiness verdict

With ADR-0030/0031 and endpoint-aware contracts in place, a team can scaffold Phase 1 without reopening whether one PeerId can serve human + Claude, how direct traffic selects a local consumer, how replies return to the correct remote/local route, whether endpoint discovery implies identity/trust, or what happens while an endpoint is offline.


## Identity recovery addendum

Software v1 identities are Ed25519 and may be backed up through the optional offline `cp2p-ed25519-bip39-entropy-v1` recovery format. The 24 words encode the exact 256-bit Ed25519 secret bytes using BIP-39 entropy/checksum/English-wordlist mapping only; Bitcoin BIP-39 PBKDF2 seed derivation is not used. Recovery is never a Channel/IPC operation.
