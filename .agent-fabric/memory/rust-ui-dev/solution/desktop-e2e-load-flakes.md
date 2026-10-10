---
role: "rust-ui-dev"
class: solution
topic: "desktop-e2e-load-flakes"
description: "j15 -- how to reproduce desktop-e2e timeouts under load, and the send-without-step defect found (Peer::deliver)"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "rust-ui-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 2f8c5fb0b06a95e5
---

## j15 -- how to reproduce desktop-e2e timeouts under load, and the send-without-step defect found (Peer::deliver)

Reproduce desktop-e2e timeouts by load, not by re-running alone: run the human_app test binary (target/debug/deps/human_app-*) 6 copies in parallel x 8 rounds under ONE tools/ci/with_display.sh, filtered to lifecycle:: retention:: daemon_link:: storage:: endpoints:: (AT-SPI cases excluded: parallel copies fight over display focus). On 6 cores, 2026-10-05: 3 failures / 48 runs, none of them the original "human lease never became held".

Defect found (fixed on branch develop-qzapp/rust-ui-dev-01/fix/desktop-e2e-waits, local until #191 merges): TransportClient::send makes ONE immediate attempt (only when Ready); every retry needs tick() -- i.e. Peer::step. Cases did peer.send then waited on A (until_rows/until_lease/a11y) without stepping B, so a first attempt refused during daemon (re)connect was never retried -> daemon_link restart case timed out twice. Fix: Peer::deliver = send + step until Accepted; Peer::state() in until's timeout message (B's daemon log records nothing about sends).
Still open: endpoints "all three accepted did not happen" (B stepped 30 s, nothing accepted) -- needs the new B-state diagnostic to explain; original lease flake unreproduced.
Related: [[desktop-e2e-stale-binary]], [[atspi-e2e-lessons]]
2026-10-05 later: with Peer::deliver, 2/48 still failed. (1) daemon_link restart: B's row Unconfirmed/NoNetworkPath for 30 s while stepped -> B's daemon did not reach restarted A; daemon stderr stops ~1 s after start-up so no why. OBSERVATION 01a10da8-5e76 to p2p-network-dev (asked: expected bound?). (2) lifecycle second-signal control: client gone <500 ms after first TERM with daemon SIGSTOPped, empty log -> added App::report() (exit status) to every running() assertion (6cb280dd). Branch fix/desktop-e2e-waits: 3 local commits. #191 dropped from merge queue 19:43 (Actions degraded, no merge_group run) -- re-arm same head when Actions healthy (owner said "arm 191").
2026-10-05 run 3 (after AFTER_A_RESTART=90 s, 13b4b973): 2/48 failed, both NEW cases: (a) retention pending-send "reaching B" timed out (no A-side info then); (b) daemon_link tap case: B's send NeedsAttention/PeerUntrusted (= UnauthorizedPeer or PeerUnknown from B's transport) although both daemons trust each other -- possible daemon start-up race, p2p's lane, NOT yet reported (one sample). Added raw last_code + A client log to failures (5th commit). p2p confirmed redial backoff 30 s doubling to 5 min (connection_manager.rs retry_backoff_ms) and took the missing gate log as a job.
Run 4 (5 commits): 1/48 -- endpoints, fresh pair (no restart): B's 3 sends NoNetworkPath/PeerUnreachable 30 s. serving() checks only IPC sockets. Did NOT add a raw TCP port probe (could count as a pre-auth failure against the shared IP). OBSERVATION 01a10db8-781b to p2p (start-up reachability + the PeerUntrusted sample); if they confirm, bound first exchange like AFTER_A_RESTART.
p2p reply 01a10db9-0be3: serving() = IPC sockets bound BEFORE runtime; "serving" INFO line logged after runtime starts; a direct send never dials (only the retry scheduler); PeerUnknown = no address yet (static provider not seeded) -- a start-up answer. Commits: d8fb4cd4 facade maps PeerUnknown -> Retry(NoNetworkPath) (was NeedsAttention/PeerUntrusted: product defect, mine); b985b3d1 two_daemons waits each daemon's "serving" line before the next. Branch now 7 commits. Run 5 started.
Runs 5+6 on b985b3d1 (7 commits): 0/96 failures (before: 3,2,2,1 per 48). Original lease timeout never seen in 240 runs; #191's session log will tell next time in CI. Branch ready to push/open after #191 merges (merge origin/main first).
2026-10-06: #191 merged 5e4ff39b (re-armed after Actions outage; Actions now flags billing overage only). j15 branch merged origin/main (50f39f4e), 2549 passed / xtask OK; opened as PR #196 (7 work commits -> arming needs owner's word; transport-client = boundary). Blind review dispatched on 50f39f4e.
#196: review (1 P3 + 1 risk) fixed a2914614/1ed224eb, re-review clean on 1ed224eb; 7 work + 2 fix; awaiting owner's word to arm.
#196 MERGED f6b61708 (2026-10-06). j15 delivered; watch CI for the lease timeout with the session log.
2026-10-07 (j26): after #208 (a send dials once, inbound resets backoff) B reaches a restarted A in 21-49 ms (12/12 under 6x load); AFTER_A_RESTART and deliver_within/until_within removed, 27f8187f on local branch test/after-a-restart. Timing via eprintln needs --nocapture (libtest swallows a passing test's stderr).
2026-10-07: j26 + j28 (#215 P3, TrustProblem::Incompatible) pushed on develop-qzapp/rust-ui-dev-01/fix/client-trust-version, 4 work commits, NO PR -- owner chose: herdr first, PR waits until more InterWeave work joins it. Commented on #215 (issuecomment-6037364807).

*References: atspi-e2e-lessons, desktop-e2e-stale-binary*

*Observed 2026-10-07 (rust-ui-dev)*
