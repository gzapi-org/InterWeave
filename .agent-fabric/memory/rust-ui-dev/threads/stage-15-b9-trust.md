---
role: "rust-ui-dev"
class: threads
topic: "stage-15-b9-trust"
description: "Stage 15 B9 trust settings + gate (b) + B8 carry -- branch feat/stage-15-trust, what landed where, evidence levels, open asks"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "rust-ui-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 37d7b8cc9fb28565
---

## Stage 15 B9 trust settings + gate (b) + B8 carry -- branch feat/stage-15-trust, what landed where, evidence levels, open asks

Branch develop-qzapp/rust-ui-dev-01/feat/stage-15-trust (2026-10-06), off origin/main 9b67201d:
- B9 (j17): client-api TrustList/TrustProblem; transport-client trust()/set_trust() each on an admin conn holding admin.trust ALONE (Recording binding test); ui-model TrustSettings + TrustInput (ViewEvent::Trust), SetTrust only via confirm(); placeholder copy (UiText Trust*); app-core Commands/Updates + turn re-renders after a trust input (defect found by the e2e: proposal never shown); ui-slint trust page (Page enum; focus lands on "Do not change"; remove buttons carry the PeerId as accessible-description); desktop log names commands only; e2e trust.rs (exact PeerId over AT-SPI, admin socket only via Tap, trust leg of a11y bullet).
- Gate (b) (j18): human_app/chat.rs -- two shipped binaries, direct+broadcast x plain+compressed both ways; receiver joins first (broadcast published once). world::two_daemons_joining, seed_pending_to, unread_rows.
- B8 carry (j19): rendered inspection under Xvfb (found + fixed unlabelled own PeerId/field, f5bc146c) and AT-SPI read of the route indicator with a SCRATCH window (label "Connected through a relay", after title/id). Real relayed path end-to-end still unproven: needs a relay pair in the desktop harness (p2p's common/).
- Allowlist is a runtime overlay lost on daemon restart (ADR-0028) -- the copy says so.
Asks for the PR: architect-cto reads placeholder copy (vocabulary), language-culture finalises. with_display cleanup race -> devex took it (branch fix/with-display-cleanup-race).
Related: [[stage-15-b6-body]], [[desktop-e2e-load-flakes]], [[slint-rendered-window-lessons]]
#200 opened 2026-10-06 (head 57d21d1f). Blind review: P1 trust page left the covered conversation "viewed" (MarkRead of unseen rows -> retention break) -- fixed daf78c78 via View::on_screen(); F2 set failure said "Nothing was changed" even if made/unconfirmed -> TrustSetFailure{NotMade,Unconfirmed,MadeNotReadBack} + take_reread (d59fcc52,d3168453,0ba0d6a7,e31669a8); F3 no raw code kept -> copy no longer says "details in diagnostics"; F4 bare SetTrust intent dropped at root; F5 one announcement per render (AnnounceBoth); F6 prose; risk Confirm(TrustChange) as shown. Copy read by architect-cto: 26 pass, NotAPeerId fixed (7b26a60e); NEW copy TrustUnconfirmed + reworded TrustFailed need their read. Reviewer/judge scratch dirs (copy/, judge/ ~2.2G) left in my scratchpad -- the review guard blocks their rm; clean after every review dispatch.
#200 ARMED 2026-10-06 at 6116efd5 (12 work + 13 fix). After rounds: re-review found a failed re-read said "Nothing was changed" -> NotReadAgain (new copy, passed); codex threads 2/3 (read-back contradicting change -> Unconfirmed; allow only against a read list + add row hidden until read) fixed. All copy passed by architect-cto (seq 13188, 13203, 13209). Gate on main since #198: 8+ work commits needs no owner's word even for a boundary.
Lesson: every review/judge dispatch leaves a build dir (scratchpad/copy, judge/, /var/tmp/review-*) its guard cannot rm -- remove it right after each report (2.2G, 897M, 17M seen).
Stage 15 close = #202 (architect-cto, owner arms). On merge: status -> stage-16-claude-code-channel; ipc/trust-list, trust-list-params, trust-set-params, path-changed ACTIVE (additive-only, ADR-0048); error_contract_matrix open_stage 16. Closing record's rust-ui-dev limits checked against my report (01a11026): accurate.
2026-10-06: trust persistence OBSERVATION (01a1123c-eb72) -> architect-cto recommends option 1 to owner (STATE-dir overlay of deltas {peer,allowed,at}; ADR-0028 amendment theirs, daemon p2p's, my settings surface marks overlay entries + drops 'until restart' copy). Waits on owner's word.

*References: desktop-e2e-load-flakes, slint-rendered-window-lessons, stage-15-b6-body*

*Observed 2026-10-06 (rust-ui-dev)*
