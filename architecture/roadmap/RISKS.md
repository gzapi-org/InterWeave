# Architecture and implementation risks

| Risk | Current treatment | Trigger / owner action |
|---|---|---|
| Claude Channel contract changes | isolated bridge + SPIKE-001 | revalidate before bridge release |
| direct v2/request-response edge behavior | explicit protocol + SPIKE-002 | pin crate version/fixtures |
| direct dedup/reservation race correctness | selector-aware key + fixed fingerprint + bounded in-flight reservation | SPIKE-002 must exercise concurrent same-key retransmission against real request-response scheduling |
| same-user IPC compromise | owner ACL + configured exclusive leases + split data/admin sockets; data socket cannot grant admin.* | SPIKE-005 if hostile same-user can access admin socket |
| endpoint squatting/collision | configured-only single lease, explicit conflict | stronger local app auth if needed |
| endpoint names mistaken for identity | contracts/UI require route-only semantics | add signed app identity above transport, not EndpointId |
| endpoint directory leaks presence | trust-gated, opt-in, active-only, max32, no labels | disable directory or add privacy scheme |
| default endpoint misroutes traffic | explicit config, validation, Accepted returns resolved endpoint | operator UI warnings/audit |
| endpoint route offline usability | no mailbox, no_route | higher-layer durable protocol only if intentionally designed |
| human app retention confused with transport durability | ADR-0044 application-only pending/unread/kept states | UI/docs/tests distinguish app survival from network acceptance/replay |
| IPC max-payload regression | fixed 128 KiB + max endpoint fixtures | compatibility review on metadata growth |
| GossipSub cross-publisher message-ID suppression | frozen source+wire-sequence `GossipSubMessageIdV1`; verify authenticity-before-valid-cache ordering | Phase-2 fixture; compatibility ADR if mapping ever changes |
| GossipSub plaintext/trust asymmetry | existing boundaries/ADR-0029 | group security/membership project if required |
| static trust scale | deliberate deny-default initial policy | signed/enterprise membership design later |
| Kademlia complexity/poisoning/privacy | trust-bounded/no-record design + opt-out | SPIKE-003/conformance/security before standard-v1 release |
| Internet reachability/NAT diversity | mandatory AutoNAT-v2 + redundant Relay-v2 + bounded DCUtR with direct-first fallback | SPIKE-004 release matrix; explicit degraded states |
| relay/probe infrastructure outage or operator abuse | independent authorized services, no application authority, quotas/failover | at least two service domains where required; operational alerts |
| unauthenticated handshake CPU/memory flood | pre-Noise pending/time/rate bounds | deployment firewall/eBPF for distributed attacks |
| trusted-peer address poisoning/backoff pollution | address-scoped failure/quarantine + known-good preference | provenance/reputation if observed in deployment |
| malicious trusted direct-request flood | per-peer/global token buckets + concurrency/queue limits | tune limits/revoke trust |
| backpressure/message loss | bounded best-effort; direct rejects before endpoint acceptance | tune/flow-control, never hidden spool |
| profile key loss | optional exact Ed25519 mnemonic recovery record; no silent regeneration | offline recovery drill; future threshold/hardware-backed identity |
| recovery phrase theft | phrase equals full PeerId impersonation capability | offline-only export/import, no IPC/logging, physical backup discipline, revoke/rotate if exposed |
| implementation sequencing bypass | ADR-0046 bottom-up gates; root dial admission before autonomous behaviours; incremental workspace activation | CI/stage review must block Kademlia/AutoNAT/Relay/DCUtR activation before lower gates pass |
| over-abstraction | EndpointRegistry concrete internal module | add trait only with real second implementation |
| software key file at rest | v1 owner-only filesystem; ADR-0038 v2.x encrypted envelope direction | SPIKE-007 selects audited format/library; HSM/keychain later |

## Human desktop/Android risks

- Android foreground-service policy/Play review changes may alter the valid always-reachable packaging; SPIKE-008 is a release gate.
- Mobile OS process suspension means pure P2P cannot guarantee offline/background reception; UI/marketing must match actual state.
- Slint platform/accessibility gaps could require a presentation-toolkit ADR change without changing human-core/transport.
- Android Keystore wrapping protects storage, not a compromised running process after seed unwrap.
- Reusing one recovery seed concurrently on multiple devices would create PeerId collision; ADR-0043 prohibits it.
- Mobile Kademlia/relay/mDNS battery usage requires tuning inside fixed protocol/security bounds, not disabling validation/trust controls.

- Android recovery UI can expose the full transport private key through screenshots/recents/IME/clipboard if platform bindings regress; secure-window + in-app mnemonic picker + no-clipboard rules are release-tested under SPIKE-008/009.
- Android system backup/device transfer can create privacy leakage or unusable half-restores if sensitive/app state is included; standard v1 disables system backup and excludes identity/config/human-store from cloud and D2D extraction. Any future explicit message backup includes only unread/receiver-kept inbound content, never pending outbound.
- `stay-reachable + user-presence` cannot automatically recover after process death; the mandatory diagnostic/UI copy prevents availability overclaiming.
