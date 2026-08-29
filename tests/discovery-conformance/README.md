# discovery-conformance

Shared DiscoveryProvider conformance applied to the cache, static and mDNS providers, plus the Stage 9 exit gate.

**Current status:** Stage 9, active test package. `shared_conformance.rs` implements the fourteen guarantees of [`architecture/contracts/DISCOVERY-CONFORMANCE.md`](../../architecture/contracts/DISCOVERY-CONFORMANCE.md) **once**, over `&mut dyn DiscoveryProvider`, and runs them against every provider — a per-provider copy is a per-provider opportunity to weaken an assertion. A deliberately misbehaving provider proves the suite catches what it claims to: emitting before start, ignoring the caller's batch bound, emitting after shutdown, forging provenance, accepting a hint it cannot honour, reporting healthy before starting, and panicking instead of returning an error.

`composition_and_exit_gate.rs` composes all three providers through a real `DiscoveryManager` and then proves the gate over real sockets: a discovered candidate for an **untrusted** peer is not remembered and not dialable, while the identical flow for a trusted peer connects. Discovery has no privileged entrance — the candidate reaches the transport through the same `add_address` any caller uses.

See [`architecture/docs/architecture/testing.md`](../../architecture/docs/architecture/testing.md) for normative scenarios and exit criteria.
