# Architecture and implementation risks

| Risk | Current treatment | Trigger / owner action |
|---|---|---|
| Claude Channel contract changes | isolated bridge + SPIKE-001 | revalidate before bridge release |
| direct v2/request-response edge behavior | explicit protocol + SPIKE-002 | pin crate version/fixtures |
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
| Kademlia complexity/poisoning | optional/default-off fully specified | SPIKE-003 before support |
| NAT limitations | conservative scope | SPIKE-004 |
| backpressure/message loss | bounded best-effort; direct rejects before endpoint acceptance | tune/flow-control, never hidden spool |
| profile key loss | explicit backup/rotation | future signed rotation/managed identity |
| over-abstraction | EndpointRegistry concrete internal module | add trait only with real second implementation |
