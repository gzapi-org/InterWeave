# Phase summary

| Phase | Focus | Key deliverables | Exit condition |
|---|---|---|---|
| 0 | spikes | Claude/direct-v2/Kademlia/NAT evidence | blocking runtime unknowns measured |
| 1 | contracts | transport v2, EndpointId, discovery/trust, config v2, IPC v2 fixtures | neutral tests compile; endpoint/config/max-payload invariants pass |
| 2 | minimal libp2p | identity, Noise/Yamux, GossipSub, direct v2, endpoint directory | explicit/default endpoint multi-peer tests pass |
| 3 | discovery | cache/mDNS/static + manager | conformance/provider isolation |
| 4 | connection policy | trust-gated dial/backoff/limits | unauthorized peers not data-plane connected |
| 5 | daemon + IPC v2 | EndpointRegistry, exclusive leases, exact direct routing, admin separation | human+Claude same-PeerId routing tests pass; no hidden buffers |
| 6A | Claude bridge | endpoint-aware Channel tools/events/replies | end-to-end Claude direct/broadcast tests pass |
| 6H | human client | IPC data plane, contacts/routes/channels, app-local history | human+Claude share PeerId without direct duplication |
| 7 | security | endpoint/trust/rate/fuzz hardening | threat-model regressions pass |
| 8 | operations | packaging/migration/diagnostics | clean update/restart/rollback |
| 9 | reachability | relay/NAT features if needed | target deployment matrix |
| 10 | Kademlia optional | private peer-routing provider/driver | SPIKE-003 + conformance/security; default still disabled |
