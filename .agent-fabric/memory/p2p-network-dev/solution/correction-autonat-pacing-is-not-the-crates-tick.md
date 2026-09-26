---
role: "p2p-network-dev"
class: solution
topic: "correction-autonat-pacing-is-not-the-crates-tick"
description: "CORRECTION to the solution slice stage-11-review-rounds.md (\"Pacing is the crate's with_probe_interval/with_max_candidates\") — since the owner's 2026-09-17 ruling (F4 on PR #84, landed by PR #88) only with_max_candidates is set from…"
tier: 2
knowledge_scope: full
distilled_at: "2026-09-26"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - c4810da1ed37c2f2
---

## CORRECTION to the solution slice stage-11-review-rounds.md ("Pacing is the crate's with_probe_interval/with_max_candidates") — since the owner's 2026-09-17 ruling (F4 on PR #84, landed by PR #88) only with_max_candidates is set from configuration; the tick stays at the crate's 5 s default and refresh_interval is the ReachabilityManager's retest cadence

The slice `.agent-fabric/memory/p2p-network-dev/solution/stage-11-review-rounds.md` (line 27 as drained 2026-09-17) says "Pacing is the crate's `with_probe_interval`/`with_max_candidates`". That was true of AUTONAT.md §4 Amendment (ii) as first written and is no longer true of the tree.

**The fact now:** the adapter sets `Config::with_max_candidates` from `connectivity.autonat.client.max_candidate_addresses_per_cycle` and leaves `Config::with_probe_interval` at the crate's 5-second default. `refresh_interval` is the `ReachabilityManager`'s cadence for returning a verified address to the sweep through ADR-0051's `retest`; it is not passed to the crate. Reason (a round-17 finding on PR #84, F4): the vendored poll issues an untested candidate only when the tick fires, so binding the tick to a 5-minute refresh quantised the 30 s retry and every first probe after a network change to 5 minutes. Consequence stated in the note: a server that connects and never answers is re-probed at the crate's fixed 5 s × `max_candidates`, and no configuration key slows that loop.

**Where:** `AUTONAT.md` §4 "Note 2026-09-17", the schema comment on `refresh_interval`, `profile-config/src/connectivity.rs` field doc, `transport/runtime/src/reachability.rs` module note — PR #88. Found by the blind reviewer of #88 as a risk outside the range; the slice is fabric-coordinator's to write, so this memory is the correction and the drain merges it.

*Observed 2026-09-17 (p2p-network-dev)*
