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
| Malicious bootstrap/Kademlia | topology bias | no authority/trust; Kademlia trust-bounded/no records/disjoint paths/budgets | compromised trusted routers | stronger diversity/membership |
| Replay | old messages reappear | 128-bit IDs + bounded endpoint-aware/broadcast dedup | outside TTL/restart | app nonce/session protocol |
| Flood/oversized | resource exhaustion | 48 KiB cap, pre-allocation checks, bounded queues/concurrency | valid-size flood | token buckets/scoring |
| Slow endpoint consumer | false acceptance/message loss | direct Accepted only after target endpoint queue admission; overload rejects | sender retries/load churn | flow control |
| Slow broadcast consumer | queue saturation | independent bounded queues/drop diagnostics | broadcast message loss | flow control |
| Local IPC attack | same-user process impersonates client | owner ACL, peer creds where available, configured-only exclusive endpoint leases, capability grants, keepalive required by default for leased endpoints | malicious same-user process remains powerful; keepalive is not authentication | capability token/keychain/sandbox |
| Endpoint squatting | local process steals `human`/`claude` route | configured-only claim + exclusive lease + client-kind hygiene | same-user attacker can spoof kind | stronger local app identity |
| Endpoint source spoof by ordinary local caller | local app claims another source route | source endpoint derived from IPC lease; not command input | compromised daemon/runtime | process isolation |
| Endpoint label used as authorization principal | implementer trusts remote `source_endpoint` such as `human`/`admin` | endpoint ACLs authorize by PeerId only; remote source EndpointId is non-authoritative routing metadata | application may misuse labels | cryptographic application/sub-identity protocol above transport |
| Remote source-endpoint spoof | peer claims `source_endpoint=human` | treat as peer-asserted route metadata only, never identity proof | applications may display misleading name | signed app/service identity above transport |
| Endpoint enumeration | remote learns local app presence | optional directory, trust-gated, advertise opt-in, active-only, max32, per-peer/global query budgets, no labels | trusted peer learns selected route names/presence | privacy-preserving presence/opaque capability IDs |
| Endpoint probing oracle | trusted peer probes route/ACL existence | unknown/offline/disabled/policy-denied collapse to coarse `no_route` | timing differences may leak | constant-work/rate policy if needed |
| Default-route confusion | messages unexpectedly hit wrong app | explicit configured default, never connection-order inference/fan-out; Accepted returns resolved route | operator misconfiguration | UI warnings/policy validation |
| Endpoint ACL widens trust | endpoint config bypasses profile allowlist | schema/runtime enforce intersection only | config bugs | invariant/property tests |
| Offline mailbox creep | implementation stores messages for absent endpoint | contract forbids daemon buffering; no Accepted without active queue | human app may separately store received history | capability-explicit durable backend only |
| Admin confused deputy | network message causes trust/endpoint mutation | admin.endpoints/admin.shutdown separated from data-plane clients; explicit local gesture | same-process GUI bug/social engineering | process split/OS auth |
| Private-key theft | attacker impersonates whole PeerId/all endpoints | owner-only key, never over IPC/logs, rotation guidance | compromise persists until revoked | hardware-backed keys |
| Recovery-phrase theft | attacker reconstructs exact Ed25519 key/PeerId | offline-only export/restore, never config/IPC/logs, explicit warning/physical custody | bearer-secret compromise persists until trust is revoked/rotated | threshold/HSM-backed recovery options |
| Recovery typo/wrong phrase | operator restores wrong identity | BIP-39 checksum + expected PeerId exact-match requirement | phrase-only disaster recovery without expected metadata has weaker typo detection | richer/versioned/threshold backup format |
| Topic enumeration/confidentiality | channel info/plaintext via trusted peers | hashed topics + trust-gated overlay + explicit no-E2EE claim | trusted forwarding peer reads payload | group E2EE |
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
