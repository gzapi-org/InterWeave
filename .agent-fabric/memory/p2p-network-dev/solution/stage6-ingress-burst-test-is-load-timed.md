---
role: "p2p-network-dev"
class: solution
topic: "stage6-ingress-burst-test-is-load-timed"
description: "two wall-clock-timed suites flake under a full workspace run on this host — tests/direct-v2 a_peer_cannot_mint_allowance_by_inventing_source_endpoints (burst + 1 vs a refilling bucket, got 34 against 33) and tests/pubsub stage7…"
tier: 2
knowledge_scope: full
distilled_at: "2026-09-26"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 4baa3e55611dedf4
---

## two wall-clock-timed suites flake under a full workspace run on this host — tests/direct-v2 a_peer_cannot_mint_allowance_by_inventing_source_endpoints (burst + 1 vs a refilling bucket, got 34 against 33) and tests/pubsub stage7 an_actual_loss_outranks_the_zero_mesh_report (timed out waiting for the local overflow report, 2026-09-18) — rerun alone before suspecting the change; a follow-up PR each

Seen 2026-09-18 during PR #89's `cargo xtask ci` on `develop-qzapp` (cargo -j 2, two test threads, a review subagent building beside it): `tests/direct-v2/tests/stage6_ingress_rate_limits.rs:351` — "and bought no MORE than the one bucket's worth, got 34". The assertion is `allowed <= PER_PEER_BURST + 1`; the per-trusted-PeerId bucket is 120/min, burst 32 (ADR-0026 amendment), so the "+ 1" tolerates one refill token — a flood of 64 frames that takes over a second under load earns two. Three reruns alone passed; PR #89's diff on that path is empty.

Also seen 2026-09-18 on PR #90's run: `tests/pubsub/tests/stage7_broadcast_over_the_wire.rs` `an_actual_loss_outranks_the_zero_mesh_report` — "timed out waiting for the local overflow report" under the full workspace run (a review subagent had just finished; two test threads); 3/3 pass alone in 0.26 s; the range touched neither pubsub nor the outbox. `cargo test --workspace` stops at the first failing binary, so a flake early in the run hides the tally — rerun with `--no-fail-fast` to get it.

**Why:** the test measures a rate limit with wall time it does not control. The right fix is a tolerance derived from the elapsed time of the flood (`burst + ceil(elapsed_s × 2)`) or a paused tokio clock, with the reason written at the assertion — not a wider constant.

**How to apply:** if it fails in a full workspace run, rerun the suite alone before suspecting the change; fix it as its own small PR in `tests/direct-v2` (mine — the workspace), citing this memory.

*Observed 2026-09-17 (p2p-network-dev)*
