# Architecture and implementation risks

| Risk | Current treatment | Trigger / owner action |
|---|---|---|
| Claude Channel contract changes | isolated bridge + SPIKE-001 | revalidate before bridge release |
| direct v2/request-response edge behavior | explicit protocol + SPIKE-002 | pin crate version/fixtures |
| direct dedup/reservation race correctness | selector-aware key + fixed fingerprint + bounded in-flight reservation | SPIKE-002 must exercise concurrent same-key retransmission against real request-response scheduling |
| same-user IPC compromise | owner ACL + configured exclusive leases + admin capability separation | SPIKE-005 if hostile same-user is in threat model |
| endpoint squatting/collision | configured-only single lease, explicit conflict | stronger local app auth if needed |
| endpoint names mistaken for identity | contracts/UI require route-only semantics | add signed app identity above transport, not EndpointId |
| endpoint directory leaks presence | trust-gated, opt-in, active-only, max32, no labels | disable directory or add privacy scheme |
| default endpoint misroutes traffic | explicit config, validation, Accepted returns resolved endpoint | operator UI warnings/audit |
| endpoint route offline usability | no mailbox, no_route | higher-layer durable protocol only if intentionally designed |
| human app history confused with transport durability | explicit application-layer boundary | UI/docs/tests must distinguish |
| IPC max-payload regression | fixed 128 KiB + max endpoint fixtures | compatibility review on metadata growth |
| GossipSub plaintext/trust asymmetry | existing boundaries/ADR-0029 | group security/membership project if required |
| static trust scale | deliberate deny-default initial policy | signed/enterprise membership design later |
| Kademlia complexity/poisoning/privacy | trust-bounded/no-record design + opt-out | SPIKE-003/conformance/security before standard-v1 release |
| Internet reachability/NAT diversity | mandatory AutoNAT-v2 + redundant Relay-v2 + bounded DCUtR with direct-first fallback | SPIKE-004 release matrix; explicit degraded states |
| relay/probe infrastructure outage or operator abuse | independent authorized services, no application authority, quotas/failover | at least two service domains where required; operational alerts |
| backpressure/message loss | bounded best-effort; direct rejects before endpoint acceptance | tune/flow-control, never hidden spool |
| profile key loss | optional exact Ed25519 mnemonic recovery record; no silent regeneration | offline recovery drill; future threshold/hardware-backed identity |
| recovery phrase theft | phrase equals full PeerId impersonation capability | offline-only export/import, no IPC/logging, physical backup discipline, revoke/rotate if exposed |
| over-abstraction | EndpointRegistry concrete internal module | add trait only with real second implementation |
