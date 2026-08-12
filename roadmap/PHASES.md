# Phase summary

| Phase | Focus | Key deliverables | Exit condition |
|---|---|---|---|
| 0 | spikes | Claude/direct-v2/Kademlia/connectivity/identity-recovery evidence | blocking runtime unknowns measured; SPIKE-003/004 release gates pass |
| 1 | contracts | transport v2, EndpointId, discovery/trust/connectivity policy, config v2, IPC v2 fixtures | neutral tests compile; endpoint/config/max-payload/connectivity invariants pass |
| 2 | minimal libp2p | identity, Noise/Yamux, Identify, GossipSub, direct v2, endpoint directory | explicit/default endpoint multi-peer tests pass |
| 3 | discovery | cache/mDNS/static/Kademlia + manager/driver | conformance/provider isolation + SPIKE-003 gates |
| 4 | connection policy | class-aware dial/backoff/limits/path ownership | unauthorized peers not data-plane connected; infrastructure-only scope enforced |
| 5 | daemon + IPC v2 | EndpointRegistry, exclusive leases, exact direct routing, admin separation | human+Claude same-PeerId routing tests pass; no hidden buffers |
| 6A | Claude bridge | endpoint-aware Channel tools/events/replies | end-to-end Claude direct/broadcast tests pass |
| 6H | human client | IPC data plane, contacts/routes/channels, app-local history | human+Claude share PeerId without direct duplication |
| 7 | security | endpoint/trust/rate/fuzz/infrastructure-class hardening | threat-model regressions pass |
| 8 | operations | packaging/migration/diagnostics/recovery UX | clean update/restart/rollback |
| 9 | **mandatory Internet reachability** | AutoNAT v2, Relay v2 reservations/server option, DCUtR, address registry/path upgrade | required NAT/relay matrix and resource/security tests pass for standard-v1 release |
