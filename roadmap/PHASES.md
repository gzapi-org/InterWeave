# Implementation phases

This roadmap starts only after architecture acceptance. Each phase should leave the repository buildable/testable and should update ADR status if evidence contradicts a decision.

| Phase | Objective | Primary deliverables | Exit criteria |
|---|---|---|---|
| 0 | compatibility spikes | Channel packaging/MCP, request-response codec, GossipSub profile, relay/NAT evidence | spike reports resolve blocking unknowns |
| 1 | neutral contracts | transport/discovery/trust/config/event types | contract tests compile; no libp2p in neutral crates |
| 2 | minimal libp2p backend | identity, TCP+Noise+Yamux, signed GossipSub, direct request-response, manual candidates | two/three-peer tests pass; PeerId persists |
| 3 | discovery framework | DiscoveryManager + cache/mDNS/static providers | conformance suite; provider failure isolation |
| 4 | connection management | address aggregation, dial/backoff/limits, reconnect | partition/recovery and storm tests pass |
| 5 | daemon + IPC | profile ownership, UDS/named pipe, multi-client event fan-out | bridge-independent daemon lifecycle; bounded queues |
| 6 | Claude Channel bridge | Channel capability, notifications, tools, instructions | external message -> Channel; send/broadcast/reply end-to-end |
| 7 | security hardening | trust admin path, rate limiting, key rotation tooling, fuzzing | rogue/flood/oversize/IPC tests pass |
| 8 | operations | diagnostics, service packaging, migration, docs | clean install/update/restart scenarios |
| 9 | connectivity hardening | relay/AutoNAT/DCUtR only as evidence requires | deployment matrix target reached |
| 10 | advanced discovery | Kademlia only if accepted after spike | poisoning/diversity/privacy criteria satisfied |
