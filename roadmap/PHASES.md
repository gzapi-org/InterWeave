# Phase summary

| Phase | Focus | Key deliverables | Exit condition |
|---|---|---|---|
| 0 | spikes | Claude/direct-v2/Kademlia/connectivity/identity-recovery evidence | blocking runtime unknowns measured; SPIKE-003/004 release gates pass |
| 1 | contracts | transport v2, EndpointId, discovery/trust/connectivity policy, config v2, IPC v2 fixtures | neutral tests compile; endpoint/config/max-payload/connectivity invariants pass |
| 2 | minimal libp2p | identity, Noise/Yamux, Identify, GossipSub with frozen source+wire-sequence message-ID function, direct v2, endpoint directory | explicit/default endpoint multi-peer and cross-publisher GossipSub-ID tests pass |
| 3 | discovery | cache/mDNS/static/Kademlia + manager/driver | conformance/provider isolation + SPIKE-003 gates |
| 4 | connection policy | class-aware dial/backoff/limits/path ownership, pre-Noise admission, address-scoped failure/quarantine | unauthorized peers not data-plane connected; poisoned address cannot peer-wide backoff a known-good trusted route |
| 5 | daemon + IPC v2 | EndpointRegistry, exclusive leases, exact direct routing, split data/admin sockets | data socket cannot acquire admin authority; human+Claude routing tests pass; no hidden buffers |
| 6A | Claude bridge | endpoint-aware Channel tools/events/replies | end-to-end Claude direct/broadcast tests pass |
| 6H-D | desktop human client | Rust/Slint UI, human-core/store, IPC data/admin adapters | human+Claude share daemon PeerId without direct duplication |
| 6H-A | Android human client | Rust/Slint UI, embedded runtime/LocalDataSession, foreground-service lifecycle, Keystore wrapping | platform-local/wire/key-custody matrix passes; final carrier-NAT/relay acceptance is the mandatory Phase-9 release gate |
| 7 | security | endpoint/trust/direct-ingress rate limits, pre-auth abuse, metadata validation/fuzz, infrastructure-class hardening | threat-model regressions and hostile trusted-peer rate tests pass |
| 8 | operations | packaging/migration/diagnostics/recovery UX | clean update/restart/rollback |
| 9 | **mandatory Internet reachability** | AutoNAT v2, Relay v2 reservations/server option, DCUtR, address registry/path upgrade | required NAT/relay matrix and resource/security tests pass for standard-v1 release |
