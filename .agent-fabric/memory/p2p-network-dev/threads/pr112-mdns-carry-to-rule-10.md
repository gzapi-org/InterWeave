---
role: "p2p-network-dev"
class: threads
topic: "pr112-mdns-carry-to-rule-10"
description: "what the next mDNS change (ADR-0053 rule 10) owes after #112 — accessor, refresh, and four carried P3s"
tier: 2
knowledge_scope: full
distilled_at: "2026-09-26"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 8ec130404094afdf
---

## what the next mDNS change (ADR-0053 rule 10) owes after #112 — accessor, refresh, and four carried P3s

#112 (ADR-0053 mDNS vendor-and-cap) MERGED 2026-09-25 16:21Z as 67dfb571 (head fbaca241), on the owner's
"merge 112 as soon as you can". The next mDNS change owes, per ADR-0053 rules 8 and 10:

- the additive live-records accessor `(PeerId, Multiaddr, expiry)`;
- the driver's 60 s refresh re-pushing what the crate holds (answers the automated
  review's standing P1 "forward record refreshes", thread 4105859023).
- the REBUILD: on `MdnsWatcherFailed`, rebuild the mDNS behaviour through the
  exported `Provider` seam (ADR-0053 rule 5 names it p2p-network-dev's, amended in
  architect-cto's PR #113, 7ff6289f; interim: no new interface until restart).

Carried P3s from the blind review of 44754742 (PR review 5319824654):
1. `mdns_driver.rs` clamp test doc names "just under the clamp" as control; the
   control is `clamp - QUERY_JITTER_MAX_MS - 1`.
2. `behaviour.rs` `watcher_dead` doc says the runtime rebuilds the behaviour;
   nothing does (Stage 12).
3. `errors_in_row = 0` reset untested: build an Err, Ok(Up loopback), Err, Pending
   watcher through the `Provider` seam; expect alive and two WatcherFailed.
4. `mdns_bounds.rs` header says every test runs in a namespace; the dead-watcher
   test does not.
5. `mdns_bounds.rs:28-29` says the send-buffer cap is unreachable while rule 4 holds; false for >=465 relevant listen addresses on one interface address (automated P2 on fbaca241, judged P3 prose).
Also open, unowned risks: the failing-interface test is wall-clock coupled; the
crate's drain loop has no per-poll budget (unmeasured).

**Why:** the owner wanted #112 merged, and each fold/fix round cost ~30 min.
**How to apply:** start the rule 10 branch off main after #112 merges; fold these
first. Related: [[arming-rule-by-pr-commit-count]].

Also carried from #117 (review fixes R1–R6, head 0f0c1daa, round 4 P3): two
orphaned doc lines in `crates/transport/libp2p/src/runtime/mod.rs` backpressure
tests, "The bound still bounds: with nothing in flight, the base capacity is the
whole allowance.", now above `the_backlog_flag_follows_the_outbox` -- move them to
the test they describe or delete them.

**Status 2026-09-26:** every item above is built on PR #120
(develop-qzapp/p2p-network-dev-01/feat/mdns-refresh-rebuild): accessor 708d3ec8, refresh
3206f583, Drop a302af30 (ADR-0053 A 2026-09-26, #119), rebuild c42dcd4c, P3s 1-5
db40b386/0f93789d/e8e417f9, #117 orphan fbf772ba. Still unowned: the wall-clock-coupled
failing-interface test and the crate drain loop's missing per-poll budget. Once #120
merges, this thread is closed except for those two.

**Carried from #120's last review (head 795c0fc0, 2026-09-26), for the next mDNS change:**
the `rebuild` and `HeldCounts` docs state the late-increment rule without naming
`mdns_bounds.rs::a_replaced_behaviours_late_counts_are_still_read` (P3, comment-only); and
the guarantee rests on `rebuild` having one caller and the refresh timer staying
`MissedTickBehavior::Delay` — nothing pins the missed-tick behaviour (risk). architect-cto owes
the record N1 (held MdnsRebuildFailed dropped on a later success) in the post-merge touch.

**#120 MERGED 2026-09-26 as 53cce603** (head 795c0fc0), armed on the owner's "merge 120"
after four review-class rounds. The rule-10/rule-8/rule-5 items are done. What stays open is
only the "Carried from #120's last review" paragraph above, plus architect-cto's record touch
(N1 and the late-count cell), sent as seq 5117.

*References: arming-rule-by-pr-commit-count*

*Observed 2026-09-25 (p2p-network-dev)*
