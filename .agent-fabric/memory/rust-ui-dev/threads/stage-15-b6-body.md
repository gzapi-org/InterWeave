---
role: "rust-ui-dev"
class: threads
topic: "stage-15-b6-body"
description: "Stage 15 B6+B7 (drawn body, one announcement, AT-SPI e2e) -- local branch state and what waits on #183"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "rust-ui-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 4c6ade67dd538c5f
  - bd57c21fa919551f
---

## Stage 15 B6+B7 (drawn body, one announcement, AT-SPI e2e) -- local branch state and what waits on #183

Branch develop-qzapp/rust-ui-dev-01/feat/stage-15-body, LOCAL ONLY (not pushed) as of 2026-10-04: c52986a1 + a20c8fa9 (body drawn, links, show source) + 71404bba/8aa31922 (F4: one announcement region, two alternating slots) + af1f1b84 (rendered-window fixes). 5 work commits; base is origin/main before #184.
Waits for #183 (B5 e2e) to merge (one open PR per agent). #183: 9 work + 7 fix, review on head 92dcd839, 0 threads, green; arm.sh REFUSED -- signals.rs is a security boundary, needs "owner's word" in --basis.
Placeholder copy added (OpenLink, ImageNotLoaded, ShowSource/Formatted, Announce*): PR must ask architect-cto to read it against the vocabulary rules, language-culture to finalise.
Open: a link label is not marked in the drawn text (design A lists controls below); one desktop-e2e flake seen once in 44 runs on #183 (lifecycle human-lease wait timed out, log not kept, unreproduced); daemon.rs admin-shutdown flake sent to p2p-network-dev (OBSERVATION 01a1081a).
Related: [[stage-15-b4-pr181]], [[slint-rendered-window-lessons]]

B7 (job j14) added on the same branch 2026-10-04: merged #183's head locally (a3983cdc); 40aa40be ui-slint select renders itself (real defect found over AT-SPI: a press yielding no intent was never drawn); 8b497609 AT-SPI driver + display focus + §13 accessibility case (trust part waits for B9); 59b2a332 reading cases (read-unkept, Keep across restart, read_pairs re-send). Branch now 8 work commits beyond #183. Mutations checked: read_pairs guard off -> re-send case fails; live region off -> accessibility case fails.

2026-10-05: #187 opened (B6+B7 + bridge bound 374807b1 from p2p-network-dev's report 01a108b8); blind review + 4 re-reviews + judged bot thread, all fixed; head 0571cc37 reviewed, 0 threads; arm.sh refuses without owner's word (root Cargo.toml comment = boundary). AT-SPI cases PASSED in CI (run 37237457316). B8 = 7 work commits local on develop-qzapp/rust-ui-dev-01/feat/stage-15-paths (ClientEvent::PeerPath, ServerState->connectivity_of, UiModel::path, header route indicator, headless test); waits for #187. j15: "human lease never became held" ~30 s under full-CI load, twice (lifecycle signal; retention process-kill per p2p-network-dev).
2026-10-05: #187 ARMED on the owner's word (head 0571cc37, queued). B8 branch develop-qzapp/rust-ui-dev-01/feat/stage-15-paths PUSHED, no PR yet (8 work commits incl. a1466c35 session log for j15); open its PR after #187 merges (merge origin/main in first). Session paused by the owner after arming.
2026-10-05: #187 MERGED 2026-10-04T23:17Z. B8 opened as PR #191 (8 work commits, origin/main merged at 0547a9c2; no Cargo.toml in diff). Local verification: xtask ci green except 4 AT-SPI cases that need with_display.sh; under the wrapper 2537 passed/0 failed. Blind review dispatched on 0547a9c2. Arming: 8 work commits = floor met; arm.sh decides boundary (transport-client?) -- if it asks, owner's word. j15 (lease flake) blocked behind #191.
2026-10-05 #191 review: P2 stale path after disconnect -> queue fail-blank (cbae80ca); root cause sent to p2p-network-dev (01a10d05-54e2), TAKEN onto their #192 (Notices::disconnected withdraws pending path notice); architect-cto writes LOCAL-CLIENT §2 sentence. CARRY: when #192 lands, REMOVE EventQueue::push's PeerPath-behind-queued-disconnect arm (it drops live paths) and its test, keep the drop-on-disconnect arm, and update transport-client README:52 + queue.rs doc. Bot (codex) thread 2: paths not cleared on session end -- being judged.
2026-10-05 later: #192 merged (f2b771ea; Notices::disconnected withdraws pending path notice; LOCAL-CLIENT §2 says so). CARRY DONE on #191: origin/main merged, queue's PeerPath-behind-disconnect refusal removed (work commit, test a_path_behind_a_queued_disconnection_is_the_path), drop-on-disconnect + drop-at-session-event kept. Also 7693ecb8/ff42fc26: paths cleared at every session event (bot thread, judged P2). #191 = 9 work + 7 fix; arm needs owner's word (transport-client = boundary in arm.json).

*References: slint-rendered-window-lessons, stage-15-b4-pr181*

*Observed 2026-10-05 (rust-ui-dev)*
