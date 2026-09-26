---
role: "p2p-network-dev"
class: solution
topic: "stage-11-step-8-dcutr"
description: "Step 8 (DCUtR) -- PR #102 MERGED 2026-09-19 (head e6fc4ef) after four blind rounds (25 commits); the crate facts, ADR-0052's DCUtR instance (the boundary as a filter by deny-and-reissue), the P3s carried to step 9's first commit, what…"
tier: 2
knowledge_scope: full
distilled_at: "2026-09-26"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 8dc62ac169c6b39f
---

## Step 8 (DCUtR) -- PR #102 MERGED 2026-09-19 (head e6fc4ef) after four blind rounds (25 commits); the crate facts, ADR-0052's DCUtR instance (the boundary as a filter by deny-and-reissue), the P3s carried to step 9's first commit, what stays open

**PR #102 ARMED/queued 2026-09-19 at `e6fc4ef`** (4 rounds; codex refused every head). Real findings per round: r1 -- a relayed inbound... no: r1 -- P2 class-gate service unpinned (wire control added), `offered` unbounded (forget on listener loss), the initiator's stalled punch dial entering the reconnect schedule (a DcutrHolePunch ticket's failure now scores/learns/schedules nothing in `record_failure` AND `settle_failed_dial`); r2 -- the withholding claim unpinned (sensor: a loopback-only subject sends an empty CONNECT, the bare responder fails NoAddresses -> its public Display is "Protocol error"); r3 -- the reissued dial's failure fell through after the hook (`reissued_in_flight`), the `-D warnings` process defect (see host-cargo memory), the swallow unpinned (sensor: the initiating end, count the bare responder's Ok events); r4 P3s carried. **ADR-0052 (architect-cto, same evening) = DCUTR.md section 6's address-class boundary**: `is_punchable_address(addr, own_listeners)` in reachability.rs (probe boundary + private admitted only beside a private listener of the same family, no source-equality); applied to what the wrapper LEARNS (NewExternalAddrCandidate withheld, counted), OFFERS (listeners), DIALS (the pending hook: the crate's dial DENIED with `DialReissued` -- the gate takes the ticket back silently -- and the survivors REISSUED as the wrapper's own dial keeping peer/Always/the hook's role; no survivor -> `refused_by_class`; the wrapper's own dial judged at the same hook = the backstop). Why deny-and-reissue: `DialOpts::get_addresses` is pub(crate) in libp2p-swarm, so the list is first visible at the hook, which can add but not remove. **Wire tests need the host's private interface** (10.137.0.2 here; `private_interface_v4()` via an unconnected UDP socket routed to 10/8): loopback candidates are refused, so the punch is made over the private pair; four dcutr.rs tests stand down with an eprintln on a host without one.

**PR #102** (`develop-qzapp/interweave/feat/dcutr-hole-punch`, cut from #101's merge `e127fd6`): `HolePunchScope` (`crates/transport/libp2p/src/hole_punch.rs`), `runtime/dcutr_driver.rs`, `SubstrateConfig.dcutr` default None, field `Toggle<ClassGated<Attributing<HolePunchScope>>>` under the DATA-PLANE gate, events `HolePunch { peer, outcome }`, `PathChange::HolePunched`, counters via `dcutr_counters()`. Wire: `tests/connectivity/tests/dcutr.rs` (2 tests, ~24 s: the success test waits a 15 s tail for the stalled dial).

**libp2p-dcutr 0.14.1 facts** (read from the crate, measured on the wire):
- The circuit's LISTENER (the reserved peer) initiates the CONNECT immediately at handler creation; the circuit's dialer waits for it. Both then dial: the initiator with `override_role()` (acts as listener in the handshake), the responder as a normal dialer.
- On loopback with listeners, the initiator's role-overridden connect lands on the responder's LISTENER and STALLS to the dial timeout; the responder's normal dial to the initiator's listener succeeds. The crate reports `Ok` only for its OWN outbound dial, so the initiator would retry (up to 3 CONNECT rounds, each a fresh direct connection) and then report `AttemptsExceeded` beside a working direct path. The wrapper ends the attempt on ANY direct connection to the peer and filters `DialFailure` once the attempt ended.
- Candidates come ONLY from `FromSwarm::NewExternalAddrCandidate` (Identify's observed address). The TCP transport reuses the listen port for dials only once a listener exists; a relay reached BEFORE the profile listened observes an ephemeral port -> the punch names a dead port. The runtime offers bound listeners as candidates at `NewListenAddr` (and on the tick). This race exists in production too (the reservation ask fires on the first tick, before Stage 12's listen) -- the listener offer covers it.
- `on_dial_failure` tracks only the initiator's attempts (`outgoing_direct_connection_attempts`); the responder learns NOTHING of a failed punch -> `ATTEMPT_HORIZON_MS = 90_000` in the wrapper. `direct_to_relayed_connections` is never pruned on a failed dial: a leak in the crate, one entry per failed punch dial (ADR-0051 patch candidate).
- `on_connection_closed` `expect`s a DIRECT connection's peer to be tracked -> a wrapper must forward a direct close only if it forwarded the establishment (ClassGated swallows the ones it denied; `HolePunchScope` tracks its own `direct` map).
- Wrapper `take_punched(connection_id)`: the Swarm consults behaviours BEFORE reporting `ConnectionEstablished`, so "in flight" is already false when the runtime sees the connection; the wrapper records the punched connection id for the runtime to read once.

**Open for step 9**: `direct_stability_period` carried in `DcutrSettings`, read by nothing; relay retirement after the upgrade; the direct-first head-start. **Carried P3s (round 4) for step 9's first commit**: `reissued_in_flight.retain(|_, p| *p != attempt.peer)` in `end()` (ClassGated can withhold the establishment on a trust change, so the entry could stay); outbound_gate.rs module note + `released_after_admission` doc naming the `DialReissued` exception; the unit test's reissued-dial hook call should pass `Endpoint::Listener`; one line in a composed wire test: `released_after_admission() == 0`. Also pre-existing/recorded: a filtered punch gets one shot where an unfiltered gets three; the crate's `direct_to_relayed_connections` leak.

*Observed 2026-09-19 (p2p-network-dev)*
