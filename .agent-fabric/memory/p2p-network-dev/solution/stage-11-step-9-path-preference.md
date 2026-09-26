---
role: "p2p-network-dev"
class: solution
topic: "stage-11-step-9-path-preference"
description: "Step 9 (path preference and stability) -- PR #103 queued 2026-09-19 at 593afe1 after 2 rounds (17 work commits); the retirement widened to any stable direct path; P3s carried to step 10's first commit"
tier: 2
knowledge_scope: full
distilled_at: "2026-09-26"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 3211f02ee117c56a
---

## Step 9 (path preference and stability) -- PR #103 queued 2026-09-19 at 593afe1 after 2 rounds (17 work commits); the retirement widened to any stable direct path; P3s carried to step 10's first commit

**PR #103** (`develop-qzapp/interweave/feat/path-preference-stability`, cut from #102's merge `96817d2`): the path derivation reads `PathSample { path, punched, stable }` per connection (`OpenConnection.punched/since_ms`; `sample(now, stability_ms)`), a punched direct ranks below a relayed until stable, `PeerPathChanged{HolePunched}` comes from the tick; `dialing::retirable` closes relayed connections behind a stable punched direct when no `pending_direct`/`pending_endpoints` names the peer (`RelayedConnectionRetired`); `HolePunchScope` counts stability failures (`HolePunch{Unstable}` + cooldown) and held upgrades; `runtime/path_race.rs` = DialPeer direct-first with the circuit deferred `RelayClientSettings.direct_head_start_ms` (from the profile's `relay.client.direct_head_start`, 750 ms; NOT a SubstrateConfig field -- only a profile with the relay transport can dial a circuit). Wire: `dcutr.rs` (2 s interval, private pair) + `tests/connectivity/tests/path_race.rs` (loopback; a black hole = a `std::net::TcpListener` never accepted: the TCP connect lands in the backlog and the Noise handshake hangs).

**Facts**: closing a punched connection at the far end IMMEDIATELY after its `dcutr::Event{Ok}` races the subject's establishment -- the subject sees nothing and the crate's next CONNECT round punches again; wait for the subject's Identify on that connection before closing (the test does). On the private pair, both punch dials can complete as separate TCP connections (no simultaneous-open merge). The libp2p Swarm cannot cancel a dial in flight.

**Lesson re-learned (2026-09-19, twice)**: `git checkout <file>` during a debug run wiped UNCOMMITTED work in that file (the wrapper's stability code); commit before any mutation or debug-print cycle that ends in a checkout. And the CI-equivalent's clippy must carry `-D warnings` (the host-cargo memory).

**Review round 1 (6 P2s) changed the design**: `DialPeer`'s reuse must read the ADMITTED CLASS (a relay's control connection is direct + infra-only); a deferred circuit's gate refusal is a `DialFailed { detail: "deferred circuit: .." }`; the retirement is behind ANY stable direct path (punched past its interval, or dialled) -- a bare far end's raw dcutr crate retries its stalled punch dial at the 10 s dial timeout, which lands a second unpunched direct connection that provided the path over the punch and defeated the punched-only rule; request-response picks a connection by request id, so a redundant circuit never idles out; the wrapper's interval start is stamped through `take_punched(conn, now)`; `OpenConnection.retiring` stops the once-per-tick report; a relayed connection closing first hands the path to the young punched direct at that close. Exchange-in-flight wire test: a bare far end with `request_response::Behaviour<StallCodec>` on `/interweave/direct/2.0.0` whose `read_request` pends forever, timeout 60 s; the subject's send runs to `DIRECT_TIMEOUT`; the relay's `closed_circuits` is the observer since `send_direct(&self)` and `next_event(&mut self)` cannot be driven together.

**Carried to step 10's first commit (round-2 P3s)**: one `now_ms(started)` per loop iteration shared by `settle_outcome`/`take_punched` and by `dcutr_driver::tick`/`sample` (a ≤2 ms straddle becomes a one-tick window at the wrapper); CONNECTIVITY.md line ~108 "retires redundant relay peer connections after a successful stable direct upgrade" is stale; `DcutrSettings::validate`'s doc omits the zero-interval rule; the retirement's "safe" ignores queued INBOUND answers (`DirectState::answering` has no peer key) -- a design note; the tick's retirement could be extracted so a unit test drives it twice (the runtime setting `retiring` is unobservable on one host).

**Open after step 9**: step 10 (network-change invalidation/recovery); SPIKE-004 phase B; a dialled direct beside a relayed one retires nothing (by design, the relayed idles out).

*Observed 2026-09-19 (p2p-network-dev)*
