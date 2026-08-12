# Final architecture review

Review posture: external CTO / implementation-readiness review. Original review 2026-08-11; amended through **mandatory Phase-9 Internet reachability and adversarial security hardening on 2026-08-12**.

## Executive assessment

**Proceed to Phase 0 spikes and Phase 1 contract scaffolding with transport/config/IPC v2 as the implementation target.** Per ADR-0045, the repository now also contains tracked implementation/test landing zones and an empty virtual Cargo workspace, but still contains no production crate/source or executable implementation.

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
| source endpoint locally spoofable? | **No by API** | runtime derives source from local-session lease (desktop IPC / Android embedded) |
| remote source endpoint over-trusted? | **No by contract** | peer-asserted routing metadata only |
| direct Accepted overclaims application processing? | **No** | sent only after local endpoint queue admission, still transport-only |
| offline endpoint creates mailbox? | **No** | no_route; ADR-0020 persists |
| endpoint directory creates discovery/trust coupling? | **No** | separate trust-gated direct control protocol, not DiscoveryProvider |
| endpoint directory leaks all local apps? | **No by default** | active + advertise=true + trust/policy filtering + max32 |
| broadcast semantics changed by endpoints? | **No** | GossipSub/ChannelId/join refs unchanged |
| Kademlia stores endpoint presence? | **No** | records remain prohibited; endpoint directory is separate |
| Discovery grants trust? | **No** | unchanged deny-default trust boundary |
| ConnectionManager policy bypass? | **No** | root dial admission including Kademlia/direct/directory/AutoNAT/relay/DCUtR origins |
| GossipSub trust mapping explicit? | **Yes** | ADR-0029 Accept/Ignore/Reject |
| GossipSub mesh message-ID cross-publisher suppression? | **Closed** | source+wire-sequence `GossipSubMessageIdV1`; application-envelope ID forbidden as the mesh key |
| local admin authority selected by spoofable client.kind? | **No** | split data/admin sockets; data socket categorically cannot grant admin.* |
| unauthenticated handshake flood bounded before PeerId? | **Yes on paper** | pending/rate/timeout pre-Noise limits + deployment firewall residual |
| poisoned trusted-peer address can poison whole peer backoff? | **No by policy** | address-scoped mismatch quarantine + known-good preference |
| IPC can carry max payload + endpoint metadata? | **By contract/test requirement** | 128 KiB IPC v2 golden fixtures |
| endpoint handshake/capability errors deterministic? | **Yes** | exact local error map in LOCAL-IPC/Phase-1 fixtures |
| direct retry race bounded/canonical? | **Yes on paper; spike required** | fixed DirectContentFingerprintV1 + 128/8 reservation limits + SPIKE-002 race test |
| recovery changes PeerId? | **No** | ADR-0033 encodes exact 32-byte Ed25519 secret and verifies expected PeerId |
| daemon/Claude/human lifecycle coupled? | **No** | desktop endpoint leases are daemon/IPC runtime; Android endpoint lease follows foreground-service session; PeerId survives ordinary UI restart |
| human admin actions can be triggered by network payload automatically? | **No by architecture** | admin authority is exposed only through the platform admin binding (desktop admin socket; Android LocalAdminPort); explicit local action required |
| hidden persistent message state? | **Closed/explicit** | transport has none; ADR-0044 limits first-party human durable content to pending outbound, unread inbound, and receiver-kept-after-read inbound |
| mandatory Internet reachability complete? | **Yes on paper; spike required** | ADR-0035 + AutoNAT-v2/Relay-v2/DCUtR state machine, path policy, limits and release tests |
| relay/probe infrastructure accidentally gains application trust? | **No by architecture** | ADR-0036 protocol-scoped connection class; data-plane protocols explicitly denied |

## Confirmed current decisions

1. Official Claude Channel architecture remains the Claude integration model.
2. Desktop/server use a separate profile-scoped daemon; Android embeds the same Rust TransportRuntime in the app foreground-service host. In both modes the runtime layer—not the UI—owns the PeerId/Swarm.
3. rust-libp2p remains first backend behind neutral contracts.
4. GossipSub remains broadcast-only with ADR-0029 validation semantics.
5. Direct remains request-response, now endpoint-aware `/direct/2.0.0`.
6. One persistent PeerId belongs to a profile.
7. Model B adds configured `EndpointId` routes beneath that PeerId.
8. One direct-capable local data-plane session owns one exclusive endpoint lease (IPC connection on desktop; embedded session on Android).
9. Direct destination is `{peer, endpoint?}`; absent endpoint means receiver default, never fan-out.
10. Receiver sends AcceptedV2 only after exact endpoint queue admission.
11. Endpoint-specific policy can only narrow profile trust.
12. Direct dedup key uses source endpoint + wire destination selector + message ID; positive entries retain the first resolved route and content fingerprint, with an in-flight reservation closing concurrent retry races.
13. Direct reply binds exact remote source endpoint and local lease epoch.
14. Endpoint directory is optional, trust-gated, active/opt-in, identity-agnostic, and bounded.
15. Human client remains above transport semantics: desktop is an IPC v2 endpoint consumer; Android embeds the same Rust TransportRuntime behind LOCAL-CLIENT rather than a second/independent libp2p stack (ADR-0032/0040/0041).
16. Human contacts/display/retention are application state above transport; message content is durable only in ADR-0044 pending-outbound, unread-inbound, or receiver-kept-after-read states.
17. Broadcast remains per-client join state; desired channels are mesh pre-warm only.
18. No persistent offline network/endpoint/Claude/human delivery store exists.
19. Static PeerId trust still gates ordinary data-plane connections, direct peers, and source admission.
20. Noise remains per-link security; trusted GossipSub forwarders can see plaintext.
21. Per ADR-0034, the standard v1 build includes Kademlia and configured entries default `enabled: true`; explicit opt-out remains supported, and Kademlia stores no app/channel/endpoint records.
22. Desktop IPC v2 remains owner-protected length-prefixed JSON with 128 KiB body and split data/admin sockets; Android bypasses fake IPC and implements the same LOCAL-CLIENT session semantics in-process. Desktop endpoint leases require negotiated keepalive by default; Android leases follow service/session lifetime.
23. Claude Channel is not granted `endpoints.query` by default; `peer_endpoints` is explicitly deferred pending a security/tool-surface revisit.
24. DirectContentFingerprintV1 is fixed byte-for-byte and direct in-flight reservation state is capped at 128 global / 8 per source peer by default.
25. Initial software identity is Ed25519 with optional offline 24-word exact-key recovery (ADR-0033); mnemonic material never crosses IPC. Verify-only drills are read-only, and full profile disaster recovery also needs a separate config.yaml backup.
26. Desktop IPC EndpointId leases require negotiated keepalive by default; Android embedded leases are revoked by service/session teardown rather than synthetic IPC keepalive.
27. Standard v1 requires AutoNAT v2 client, Circuit Relay v2 client/reservations, and DCUtR (ADR-0035); Phase 9 is a release requirement, not optional hardening.
28. Relay/AutoNAT service peers may use the ADR-0036 connectivity-infrastructure class, which permits only control-plane protocols and never grants GossipSub/direct/endpoint/Kademlia application authority.
29. Reachability uses multi-observer AutoNAT evidence, redundant relay reservations, direct-first path selection and bounded DCUtR upgrades with relay fallback.
30. GossipSub duplicate identity is source+wire-sequence bound and versioned; two publishers may safely reuse the same application envelope ID without mesh-level suppression collision.
31. IPC administration uses a separate admin socket; `client.kind` is never the authority selector and the data socket cannot grant admin.*.
32. Internet listeners apply pre-Noise pending/rate/time bounds before PeerId exists, while trusted direct peers also face per-peer/global ingress token buckets.
33. Dial failure state distinguishes address failures/identity mismatches from peer punitive backoff; a poisoned address cannot suppress a known-good trusted route by itself.
34. Remote AcceptedV2/endpoint-directory metadata is grammar/bound/TTL validated before cache/tool/UI exposure.
35. First-party human UI/application logic is Rust with Slint as the reference desktop/Android presentation layer (ADR-0039).
36. Concurrent desktop/Android human devices use distinct PeerIds; mnemonic restore is recovery/migration, not cloning (ADR-0043).
37. Android wraps the exact portable Ed25519 seed with Android Keystore AES-GCM and exposes explicit background-compatible/user-presence unlock policies (ADR-0042).
38. AutoNAT server dial-back is source-IP restricted; Identify infrastructure candidates default off; DCUtR upgrades emit PeerPathChanged rather than duplicate PeerConnected.
39. First-party human UI never upgrades transport acceptance into read/seen semantics and never treats display names, EndpointIds, or contact grouping as authenticated human identity.
40. Desktop packaging preserves three roles (`human-desktop`, `transport-daemon`, `transportctl`); Android bundles UI/runtime in one application/service lifecycle while preserving the same network and local-session contracts.
41. Human message retention is application-scoped and ephemeral-by-default: pending outbound + unread inbound survive locally; inbound survives after read only when the receiver explicitly chooses Keep; transport-terminal outbound/read-unkept content evaporates (ADR-0044).

## Accepted limitations

- no network offline mailbox;
- no exactly-once/global order;
- EndpointId does not prove person/application identity;
- same-user malicious desktop process that can open the admin socket remains partly inside the IPC residual boundary; Android same-process compromise remains inside the embedded application trust boundary;
- endpoint-directory advertisement leaks selected presence to trusted peers;
- endpoint directory can be stale;
- static trust does not scale to public networks;
- no group E2EE;
- no guarantee that every NAT permits a direct DCUtR path; standard v1 nevertheless requires relay fallback, and loss of all authorized relays can still isolate an inbound-private peer;
- relay/probe operators can observe connectivity metadata and deny service; the system is not an anonymity network;
- default-on Kademlia increases ordinary metadata/topology/privacy exposure and therefore makes SPIKE-003/conformance/security a standard-v1 release gate;
- a human client persists only pending outbound, unread inbound, and receiver-kept inbound; it cannot recover messages never accepted while it was offline, so Android process/service absence remains real offline state;
- the BIP-39-derived recovery UX has only an 8-bit mnemonic checksum, so expected-PeerId backup metadata is the stronger restore check;
- recovery phrase theft is full PeerId private-key compromise.
- v1 has no authenticated human-account identity or cross-device history synchronization; contacts may group several device PeerIds only as local application metadata.

## Remaining implementation risks / spikes

### Claude/MCP version skew

SPIKE-001 remains blocking before production bridge packaging.

### Direct v2 asynchronous acceptance

SPIKE-002 must verify request-response protocol-family negotiation/failure behavior, the practical pattern for withholding AcceptedV2 until bounded runtime endpoint queue admission, concurrent same-key retransmission against the real request-response scheduler, and the pinned GossipSub authenticity-before-valid-duplicate-cache ordering. It must also prove two authenticated publishers reusing one application-envelope message ID remain distinct under `GossipSubMessageIdV1`. It may adjust task/channel mechanics, not endpoint routing/dedup or mesh-ID semantics.

### Kademlia

SPIKE-003 is required before the standard v1 build ships configured Kademlia entries default-enabled. Failure blocks/revisits ADR-0034 rather than silently shipping an unsupported default.

### Mandatory Internet reachability

SPIKE-004 is a **standard-v1 release/tuning gate** for the already-selected AutoNAT-v2/Relay-v2/DCUtR architecture. It must validate behaviour-originated dial admission, infrastructure-only protocol isolation, redundant reservations, address advertisement, direct-first racing, hole-punch fallback, server-role quotas, network-change recovery and resource budgets on the pinned rust-libp2p release. Failure blocks standard-v1 release or requires ADR-0035/0036 to be superseded; it does not silently make Phase 9 optional.

### Same-user local client authentication

SPIKE-005 remains conditional. Model B endpoint leases improve routing/isolation but do not cryptographically authenticate same-user client executables.

### Identity-recovery portability

SPIKE-006 must verify that the pinned rust-libp2p Ed25519 identity API/portable serialization boundary round-trips the exact 32-byte secret assumed by `interweave-ed25519-bip39-entropy-v1` and reproduces the same PeerId. Failure keeps production mnemonic backup/restore disabled; it does not authorize silently changing the recovery format.

### Android execution / platform policy

SPIKE-008 is required before shipping Android stay-reachable mode. It validates the current `remoteMessaging` foreground-service classification, target-SDK/Play-policy requirements, lifecycle/background-start behavior, process/network recovery, secure recovery-window/task-snapshot behavior, Android backup/device-transfer exclusions and honest offline states.

### Android key custody

SPIKE-009 is required before shipping Android production key storage. It validates AndroidKeyStore AES-GCM wrapping of the exact Ed25519 seed, both unlock policies, the in-app no-clipboard mnemonic flow and the `background_restart_requires_user_authentication` diagnostic without changing the PeerId fixture.

## No-production-implementation verification

Expected content remains Markdown/YAML architecture + Git metadata. No Cargo workspace, `.rs`, production MCP server, human client executable, daemon, installer, service unit, or identity key should exist.

## Implementation-readiness verdict

With endpoint-aware contracts and ADR-0035/0036 in place, a team can scaffold Phase 1 without reopening Model-B routing or the Internet-reachability architecture. The remaining empirical work is library integration/tuning: the standard-v1 requirement is already fixed as AutoNAT-v2 evidence + Relay-v2 fallback + bounded DCUtR direct upgrades under class-aware root dial admission.


## Identity recovery addendum

Software v1 identities are Ed25519 and may be backed up through the optional offline `interweave-ed25519-bip39-entropy-v1` recovery format. The 24 words encode the exact 256-bit Ed25519 secret bytes using BIP-39 entropy/checksum/English-wordlist mapping only; Bitcoin BIP-39 PBKDF2 seed derivation is not used. Recovery is never a Channel/IPC operation.

### Optional encrypted software-key-at-rest path

ADR-0038 makes a passphrase-encrypted exportable software-key envelope an explicit v2.x option rather than an unnamed future possibility. SPIKE-007 must select an audited maintained format/library and unlock UX. Standard v1 remains owner-permission-protected plaintext portable key storage; this is an accepted at-rest limitation, not a claim of disk-compromise resistance.
