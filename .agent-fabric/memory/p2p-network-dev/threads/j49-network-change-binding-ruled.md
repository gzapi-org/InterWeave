---
role: "p2p-network-dev"
class: threads
topic: "j49-network-change-binding-ruled"
description: "§20 step 5 runtime side (j49) ruled by architect-cto seq 33736: NetworkView snapshot of IPs into the ONE detector, one redial per allowlisted peer on an addition, wildcard-only listeners for embedded-android"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - b161f747fef3243f
---

## §20 step 5 runtime side (j49) ruled by architect-cto seq 33736: NetworkView snapshot of IPs into the ONE detector, one redial per allowlisted peer on an addition, wildcard-only listeners for embedded-android

Proposal seq 33720, ruling seq 33736 (2026-10-09):
- `EmbeddedHost::network_changed(NetworkView { addresses: Vec<IpAddr> })`, non-blocking SNAPSHOT of every usable address; empty = offline; before the first view = unknown, runtime runs on listener polling. Addresses only (no network ids/ifnames/carrier: ADR-0054 privacy). Idempotent; same set = no change; Service passes a view only when addresses differ; say in the design what happens on a new network id with equal addresses (SPIKE-008 L7) -- keepalives prove connections.
- Both sources (view + listener bound set) feed the ONE detector in transport/libp2p runtime/network_change.rs; change reported only when the known IP set moves; existing §14 handling (mod.rs ~2858) reused.
- On a change that ADDS an address: every allowlisted not-connected peer due at once, one redial per peer per change through the root gate, backoff resumes from that dial's result. Not on removal, not per poll tick. Control test: no view -> peer waits its backoff.
- profile-config validation row: embedded-android listeners wildcard only, refused at load by name.
- Docs (CONNECTIVITY.md §14, human-client-android.md, config schema comment) are architect's supply onto my branch once code exists.
- Facts found: if-watch 3.2.2 uses its polling fallback on target_os=android (getifaddrs every 10 s); netlink backend is linux-only. Device run of getifaddrs under Android 11 joins step 1 evidence; if it answers nothing, the view is the only source -- design must say so.
- Sequencing: j48 (#241) lands first; j49 detector/backoff/validation can branch off origin/main, the EmbeddedHost wrapper needs #241 merged.

BUILT 2026-10-09 on develop-qzapp/p2p-network-dev-01/feat/network-change-binding (pushed 915491ba, no PR): 6 work + 2 carried Answers (#241 P3s). Detector = one IP set, per-source deltas (first view removes nothing it never named); network_added lifts DataPlaneTrusted peer backoff + makes retry due, attempts kept; NetworkMonitor = watch slot; AndroidListenerNotWildcard; NetworkChanged carries Vec<IpAddr>. Every new test mutation-checked.
Open: open the PR only after dev-02's j6 PR merges (it changes connection_manager Retry: waits_on/relay_reached) -- MERGE origin/main in (branch is published, no rebase); architect docs supply asked seq 55247 (CONNECTIVITY §14, android doc, schema row, CLAUDE.md:204 departed_ips, ADR-0054 citation wrong, limits: relay ladder not made due on addition; port-only change no longer invalidates AutoNAT). rust-ui-dev told the Service call (seq 55342).
Lesson: watch::Sender changed() arm needs Ok(()) pattern. A test helper that borrows &mut and returns a future MUST be a module-level async fn -- a closure returning async move fails "lifetime may not live long enough" (hit 3 times on j49); write the async fn first.
2026-10-09 later: architect supplied docs cf925f98 (ADR-0001 is the addresses-only record) and ruled the relay ladder due on addition (seq 55562): built 7efacc6b/90af0da5/591d29d8. Finding: the ladder alone failed at the gate -- the offline dial left the relay PEER in ConnectionManager peer backoff; 90af0da5 lifts gate backoff for every classified peer (retry due only for data-plane). Decision asked seq 57432 (their §14 says infra peers untouched). Branch now 12 commits (10 work by count incl. supply? check pr-gate). CI started on 591d29d8.
2026-10-09 evening: blind review of 0cf71c2f..591d29d8 (no P1); rulings at architect 60ec1be2 (ADR-0011 A 2026-10-09: lift floor 30 s/retry_min, first fill lifts, view authoritative). Built 99a048dc (floor), de611722 (first fill, view authority, holds filter at every offer site, latest-wins test), d63b2a4d (F7); all mutation-checked. Re-review of 591d29d8..d63b2a4d dispatched. Supply for dev-02's #246 F2 pushed ecba75e7 (empty relay route refused by name). PR still waits for dev-02's PR to merge (connection_manager overlap).
CI note: test_rust-android self-test fails on this account (no aarch64 target, no rustup) and human-chat linearity is load-timed -- neither is this range.
PAUSED 2026-10-09 ~19:10Z by the owner (host restart; fabric-coordinator seq 75835). Branch pushed at e70747ce, clean. Folded architect's supply 2fa69380 (merge c88908ba): ADR-0011/0052 A 2026-10-09 rulings.
Done after the re-review: N1/N3/N4 prose (9bb9975d, 18eef8f3, 6e683189); gap B relay ladder skips a relay the gate still holds (af803ee3 holds_off + 35029a68 wiring, wire test an_addition_inside_the_gates_floor_leaves_the_relay_ladder_alone); gap A just-bound listener offered after observe, if held (e70747ce, dcutr test extended).
RESUME WITH: (1) mutation-check gap B wiring (pass |_| false in mod.rs lift_held_off -> wire test must fail) and gap A (drop .filter(holds) on just_bound -> dcutr test must fail); (2) gap C, architect seq 74818: ADR-0052 rule 3 own-listener test reads bound AND known IP at EVERY instance -- RootFunnel (NewListenAddr/ExpiredListenAddr own listeners), Kademlia set_own_listeners, relay state set_own_listeners, commands.rs:535/1129, mod.rs own_listeners sites (~2554/2600/2639/2706/2786) -- with tests; (3) pre-existing: AutoNAT holds filters (mod.rs ~187 change turn, ~2210 tick) have no test; add one; (4) tell architect the shas for A/B/C so they append dated paragraphs; (5) full xtask ci, merge origin/main after dev-02's PR merges (connection_manager overlap), open PR with counts, blind review of the whole range.
RESUMED 2026-10-10: gap A/B mutations caught; gap C done 7c1eaf7b (HeldListeners shared record published by NetworkSet, RootFunnel ctor arg; NetworkSet::own_listeners at every runtime rule-3 site incl. mdns_tick own param and commands.rs Learn/OfferRoutingPeer; wire test a_departed_ips_listener_admits_no_private_candidate); architect told seq 91460. AutoNAT holds filters untestable on this host (only probeable/public listeners offered). Remaining: xtask ci on 7c1eaf7b, wait for dev-02's PR merge, merge main, open PR with counts, blind review of the whole range.
2026-10-10: merged main (1ee90a9c, ADR same-day amendments kept both; 0011 body order network-addition before relay-hop, architect agreed); re-review 2 fixes b2f154c5/22d6498f (funnel wiring test kademlia_driver a_walk_after...)/a605e63a; xtask ci GREEN on a605e63a. PR #250 OPEN (21 work, 10 fix), body has supply range lines + AWAITING-SUPPLY: architect-cto-01 (their j75: strike "owed/at this writing" clauses; they cut from a605e63a when their pause lifts) -- add its range line when folded, then re-review that range before arming. Blind review dispatched in two parts (A substrate, B surroundings) on a605e63a; post both with post-review.sh, answer findings on the PR.
2026-10-10 later: two-part blind review of a605e63a posted on #250 (no P1; B-F1 P2 = architect j75). Judged and fixed: B-F2 expired backoff not a lift + B-F3 claimed retry guard (f5730cb1), A-F1/A-F3/B-F4/A-F2 docstring (8f95064a, 8c752c68, 22f31edb, ccfb645e, 11ffc4ad); dispositions comment posted; PR body limits gained the DCUtR crate candidate-cache gap (pre-existing follow-up: rebuild dcutr inner on removal). Re-review of a605e63a..11ffc4ad dispatched. Still: architect j75 supply -> add range line, re-review it, then arm via arm.sh.
Re-review of a605e63a..11ffc4ad posted (no P1/P2); its P3 fixed in code (network_lifts pruned by class too) + LRU nit, pushed a8bed51b. #250 (21 work, 18 fix) unarmed. NEXT: when architect's j75 supply lands -> fold, add range line under the AWAITING-SUPPLY in the PR body, dispatch ONE re-review of 11ffc4ad..<new head>, post, then arm.sh once checks are all SUCCESS.

*Observed 2026-10-09 (p2p-network-dev)*
