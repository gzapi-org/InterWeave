# Threat model

Scope: generic transport, profile daemon/local clients, endpoint routing, and network participation. Application-level human identity/authorization remains outside transport.

Assumption: every remote payload is untrusted even after PeerId is allowlisted.

Trust boundaries:

1. remote network -> Noise/libp2p parser;
2. Noise-authenticated PeerId -> profile PeerTrustPolicy;
3. trusted direct source -> EndpointId route/policy admission;
4. admitted payload -> limits/dedup/local queue;
5. daemon -> owner-protected capability-scoped IPC + endpoint lease;
6. bridge/human app -> application handling;
7. local administrator -> trust/key/endpoint/config mutation.

| Threat | Risk | Mitigation | Residual risk | Future mitigation |
|---|---|---|---|---|
| Rogue peer | injects traffic/data-plane | Noise + deny-by-default profile allowlist before ordinary data-plane/directory | stolen/incorrectly trusted PeerId | signed membership/enterprise policy |
| Discovery poisoning | bogus addresses waste dials | advisory discovery, caps, trusted dial admission | trusted stale/poisoned address | source reputation/diversity |
| Trusted-PeerId address poisoning/backoff pollution | attacker advertises wrong address under a trusted PeerId so Noise fails/mismatches and whole peer is suppressed | address-scoped backoff/quarantine, known-good address preference, identity mismatch penalizes address not expected PeerId | many poisoned addresses can still consume bounded dial budget | stronger provenance/reputation and signed address assertions if needed |
| Malicious bootstrap/Kademlia | topology bias | no authority/trust; Kademlia trust-bounded/no records/disjoint paths/budgets | compromised trusted routers | stronger diversity/membership |
| Replay | old messages reappear | 128-bit IDs + bounded endpoint-aware/broadcast dedup | outside TTL/restart | app nonce/session protocol |
| Pre-Noise handshake flood | unauthenticated TCP/Noise setup burns CPU/memory before PeerId trust exists | pending-handshake caps, 10 s timeout, per-source/global start-rate limits, backend connection limits | distributed-source floods/NAT collateral; source IP may be unavailable on some paths | deployment firewall/eBPF/OS SYN controls |
| Flood/oversized | resource exhaustion | 48 KiB cap, pre-allocation checks, bounded queues/concurrency, mandatory per-trusted-PeerId/global direct token buckets | valid broadcast/control flood | GossipSub scoring/additional protocol-specific limits |
| Slow endpoint consumer | false acceptance/message loss | direct Accepted only after target endpoint queue admission; overload rejects | sender retries/load churn | flow control |
| Slow broadcast consumer | queue saturation | independent bounded queues/drop diagnostics | broadcast message loss | flow control |
| Local IPC attack | same-user process impersonates client | owner ACL, peer creds where available, **separate data/admin sockets**, configured-only exclusive endpoint leases, keepalive required by default for leased endpoints; data socket can never grant admin.* regardless of client.kind | malicious same-user process that can open the admin socket remains powerful; keepalive/client.kind are not authentication | SPIKE-005 stronger local credential/user-presence mechanism; stricter OS account/group ACL |
| Endpoint squatting | local process steals `human`/`claude` route | configured-only claim + exclusive lease + client-kind hygiene | same-user attacker can spoof kind | stronger local app identity |
| Endpoint source spoof by ordinary local caller | local app claims another source route | source endpoint derived from IPC lease; not command input | compromised daemon/runtime | process isolation |
| Endpoint label used as authorization principal | implementer trusts remote `source_endpoint` such as `human`/`admin` | endpoint ACLs authorize by PeerId only; remote source EndpointId is non-authoritative routing metadata | application may misuse labels | cryptographic application/sub-identity protocol above transport |
| Remote source-endpoint spoof | peer claims `source_endpoint=human` | treat as peer-asserted route metadata only, never identity proof | applications may display misleading name | signed app/service identity above transport |
| Endpoint enumeration | remote learns local app presence | optional directory, trust-gated, advertise opt-in, active-only, max32, per-peer/global query budgets, no labels | trusted peer learns selected route names/presence | privacy-preserving presence/opaque capability IDs |
| Endpoint probing oracle | trusted peer probes route/ACL existence | unknown/offline/disabled/policy-denied collapse to same `no_route` code/shape/shared encoder; direct per-PeerId rate limit | timing differences may still leak because constant-time route/policy evaluation is not promised | revisit constant-work policy only if measured leakage justifies cost |
| Remote transport-metadata injection | malicious trusted peer sends invalid `AcceptedV2.resolved_endpoint` or endpoint-directory fields that reach Claude/UI/cache | grammar/length/message-id validation before surfacing, explicit-route equality check, directory <=32/unique validation, TTL clamp/local receipt aging | valid peer-controlled endpoint labels can still mislead humans/apps | higher-layer authenticated application identity |
| Default-route confusion | messages unexpectedly hit wrong app | explicit configured default, never connection-order inference/fan-out; Accepted returns resolved route | operator misconfiguration | UI warnings/policy validation |
| Endpoint ACL widens trust | endpoint config bypasses profile allowlist | schema/runtime enforce intersection only | config bugs | invariant/property tests |
| Offline mailbox creep | implementation stores messages for absent endpoint | contract forbids daemon buffering; no Accepted without active queue | human app may separately store received history | capability-explicit durable backend only |
| Admin confused deputy | network message causes trust/endpoint mutation | admin methods exist only on separate admin socket; data socket categorically denies admin.*; explicit local gesture | same-process GUI bug/social engineering; malicious same-UID process may open admin socket | SPIKE-005 stronger OS auth/user presence |
| Private-key theft | attacker impersonates whole PeerId/all endpoints | owner-only key, never over IPC/logs, rotation guidance | plaintext key remains readable to same-user compromise/disk theft where OS storage is exposed | ADR-0038 optional v2.x audited passphrase-encrypted envelope or hardware-backed keys |
| Recovery-phrase theft | attacker reconstructs exact Ed25519 key/PeerId | offline-only export/restore, never config/IPC/logs, explicit warning/physical custody | bearer-secret compromise persists until trust is revoked/rotated | threshold/HSM-backed recovery options |
| Recovery typo/wrong phrase | operator restores wrong identity | BIP-39 checksum + expected PeerId exact-match requirement | phrase-only disaster recovery without expected metadata has weaker typo detection | richer/versioned/threshold backup format |
| Topic enumeration/confidentiality | channel info/plaintext via trusted peers | hashed topics + trust-gated overlay + explicit no-E2EE claim | trusted forwarding peer reads payload | group E2EE |
| GossipSub message-ID suppression | trusted peer races/reuses another publisher's envelope ID to poison mesh duplicate cache | frozen `GossipSubMessageIdV1` binds signed source PeerId + signed wire sequence number and never keys on the application envelope ID; Phase-2 cross-publisher and pre-cache-authenticity fixtures | malicious publisher can still suppress/replace its own repeated ID semantics | higher-level signed sequencing if applications need stronger replay/order guarantees |
| GossipSub trust asymmetry | mesh propagation partition | ADR-0029 Ignore vs Reject mapping | downstream route loss | shared membership policy |
| Prompt injection | remote content asks unsafe actions | Channel instructions/normal permissions/no admin tools | model/user can choose action | sandbox/app policy |
| Address SSRF-like abuse | hostile multiaddr dials sensitive network | validation, allowed transports, trust/dial policy | trusted peer can point private ranges | egress policy |
| Log leakage | route/peer/content secrets logged | payload logging false, redaction, bounded diagnostics | endpoint names may be sensitive | pseudonymization |

## Endpoint identity boundary

The authenticated principal is the PeerId. EndpointId is subordinate routing metadata:

```text
Noise proves:        remote PeerId controls this connection
Direct v2 claims:    source_endpoint = "human"
Transport does NOT prove: a particular human/application/role owns that endpoint
```

A human client must keep display identity/contact verification above this boundary.

## Endpoint directory privacy boundary

Directory results are authenticated as statements from the trusted remote PeerId but are not signed sub-identities. Only active, opt-in routes are returned. Directory cache is short-lived and non-persistent.

## Broadcast confidentiality boundary

Transport does not rely on topic-name secrecy. Trusted forwarding peers can read plaintext unless a higher layer encrypts payloads. Endpoint addressing does not change this because EndpointId is absent from transport GossipSub envelopes.

## Security non-goals

- proving person/application identity from PeerId or EndpointId;
- group E2EE;
- anonymous routing/metadata privacy;
- Byzantine consensus/membership;
- durable replay prevention;
- protecting against fully malicious same-OS-user code;
- offline endpoint delivery.

## Kademlia-specific threat treatment

Kademlia is enabled by default for configured entries in the standard v1 build. Therefore poisoning/Sybil/eclipse/bootstrap/namespace/record-store/query-privacy mitigations are part of the normal threat posture, not an optional-feature posture. Explicit `enabled: false` remains available for profiles that opt out. Endpoint IDs and endpoint presence are never written into Kademlia provider/value records; endpoint discovery stays on the separate trust-gated direct endpoint-directory protocol.


## Mandatory Internet reachability threat boundary

Phase 9 introduces infrastructure that affects availability and metadata but does not become application trust. ADR-0036 defines a separate connectivity-infrastructure class.

| Threat | Attack | Mitigation | Residual risk |
|---|---|---|---|
| malicious/compromised relay | drop/delay circuits; correlate PeerIds, timing, relay use | Noise-authenticated end-to-end peer connection through relay; redundant authorized relays; relay never grants application trust | relay still observes connection metadata and can deny service |
| malicious AutoNAT observer | lie about reachability or selectively fail probes | require recent successful evidence from multiple distinct authorized probe servers; evidence expires; retain relay fallback | colluding observers can bias reachability classification |
| infrastructure privilege escalation | relay/probe PeerId tries GossipSub/direct/endpoint/Kademlia | protocol-scoped connection class; root dial admission; GossipSub exclusion; direct/endpoint/Kademlia admission checks | implementation bug in shared Swarm policy remains security-critical |
| relay exhaustion / abusive reservations | consume relay capacity/circuit bandwidth | bounded reservations/circuits, per-peer/global quotas, duration/byte caps and rate limits | sufficiently large distributed abuse can still cause denial of service |
| hole-punch abuse | trigger repeated dials/address probing | trusted application destination only, global/per-peer DCUtR bounds, cooldown, root dial admission | peers learn candidate network addresses required for connectivity |
| stale relay address advertisement | peers dial expired reservation | advertise only active reservations; remove immediately on reservation loss; normal retry/address merge | distributed caches may retain stale addresses temporarily |

The system does **not** promise anonymous routing, universal direct hole-punch success, or availability when every authorized relay/probe service is unreachable. Relay transport preserves authenticated encrypted peer sessions but does not hide PeerIds or traffic timing from relay operators.
