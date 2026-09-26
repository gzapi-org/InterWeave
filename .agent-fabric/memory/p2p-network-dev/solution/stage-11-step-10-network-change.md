---
role: "p2p-network-dev"
class: solution
topic: "stage-11-step-10-network-change"
description: "Step 10 (network change) -- PR #104 MERGED 2026-09-19 on the owner's word after 3 rounds; Stage 11's list is complete (no step 11), the stage stays open on SPIKE-004 phase B; Stage 12 (composition root) awaits the owner's word; the last…"
tier: 2
knowledge_scope: full
distilled_at: "2026-09-26"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - d49ddc52e89baea2
---

## Step 10 (network change) -- PR #104 MERGED 2026-09-19 on the owner's word after 3 rounds; Stage 11's list is complete (no step 11), the stage stays open on SPIKE-004 phase B; Stage 12 (composition root) awaits the owner's word; the last step of Stage 11's list -- the stage cannot close (SPIKE-004 phase B) and the next stage is the owner's to open

**PR #104** (`develop-qzapp/interweave/feat/network-change-recovery`, cut from #103's merge `30728dc`): `runtime/network_change.rs` (`NetworkSet::observe` over the bound set minus interface-scoped addresses, after the first bind; every later difference including empty<->filled) evaluated in the Swarm-event arm right after `translate` for `NewListenAddr|ExpiredListenAddr|ListenerClosed`; `autonat_driver::network_changed` (verdict unknown + publish -> `follow_verdict`, `reset_listeners`, `retest_all(now)` = every schedule entry `due = now + rand(0..=NETWORK_CHANGE_JITTER_MS=5 s)`, failures reset; the adapter's own listener comparison and `state.listeners` REMOVED); `HolePunchScope::network_changed` (attempts Abandoned, cooldown/stabilising/offered cleared); `SwarmEvent::NetworkChanged { removed, added }`. First commit closed #103's round-2 P3s (one `now` per loop iteration; CONNECTIVITY.md policy-owner bullet; validate's doc).

**Facts**: the adapter's unit tests that drove a listener change through `reconcile` now call `network_changed()` explicitly (the wrapper's listener set only restarts there -- a departed offered listener otherwise stays in the wrapper's candidate list and changes the truncation counts). On this host the AutoNAT half is unobservable on the wire (no evidence to invalidate); the wire test (`dcutr.rs::a_network_change_lifts_the_cooldown_and_keeps_the_reservation`) uses a private-ip listener stopped via `stop_listening` -- loopback listeners are interface-scoped and never a change.

**Review rounds 1-3 (P2 + 9 P3s, all fixed here)**: a given-up attempt keeps its permit (`Attempt.abandoned`; `end` maps everything but `Succeeded` to `Abandoned`); only a REMOVED address invalidates (`NetworkChange::invalidates`); the adapter re-offers bound listeners in `network_changed`. Wire pins in `dcutr.rs`: `a_network_change_lifts_the_cooldown_and_keeps_the_reservation` (addition then removal), `a_network_change_keeps_a_given_up_attempts_permit_until_the_crate_is_done`.

**After step 10**: Stage 11's list is complete; its exit gate (the NAT matrix, SPIKE-004 phase B) cannot pass on this host; do NOT open Stage 12 without the owner's word. #98 (5 commits) and #97 (architect-cto's ADR) still open.

*Observed 2026-09-19 (p2p-network-dev)*
