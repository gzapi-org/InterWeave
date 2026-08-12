# Implementation phases

This roadmap starts only after architecture acceptance. Each phase should leave the repository buildable/testable and should update ADR status if evidence contradicts a decision.

| Phase | Objective | Primary deliverables | Exit criteria |
|---|---|---|---|
| 0 | compatibility spikes | Channel packaging/MCP, request-response codec, GossipSub profile, relay/NAT evidence | spike reports resolve blocking unknowns |
| 1 | neutral contracts | transport/discovery/trust/config/event types + IPC golden fixtures | contract tests compile; no libp2p in neutral crates; exact max payload fits 128 KiB IPC body; Kademlia cross-field/seed-source config rules pass |
| 2 | minimal libp2p backend | identity, TCP+Noise+Yamux, signed GossipSub with explicit validation results, direct request-response, manual candidates | trusted multi-peer tests + Reject/Ignore/Accept cases pass; PeerId persists |
| 3 | discovery framework | DiscoveryManager + cache/mDNS/static providers | conformance suite; provider failure isolation |
| 4 | connection management | address aggregation, trust-gated dial/inbound retention, backoff/limits, reconnect | unauthorized peers are not data-plane connected; partition/recovery and storm tests pass |
| 5 | daemon + IPC | profile ownership, UDS/named pipe, 128 KiB framed JSON, capability-scoped multi-client event fan-out | bridge-independent daemon lifecycle; direct all-client/broadcast-interest fan-out tests pass; no hidden buffer; Channel client cannot shutdown daemon |
| 6 | Claude Channel bridge | Channel capability, notifications, tools, instructions | external message -> Channel; trust/join-aware send/broadcast/reply end-to-end |
| 7 | security hardening | trust admin path, rate limiting, key rotation tooling, fuzzing | rogue/flood/oversize/IPC tests pass |
| 8 | operations | diagnostics, service packaging, migration, docs | clean install/update/restart scenarios |
| 9 | connectivity hardening | relay/AutoNAT/DCUtR only as evidence requires | deployment matrix target reached |
| 10 | optional Kademlia discovery | implement private peer-routing provider/driver; default remains disabled | SPIKE-003 + conformance + 20-node/security matrix |
