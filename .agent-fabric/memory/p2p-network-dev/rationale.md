---
role: "p2p-network-dev"
class: rationale
description: "cargo-mutants was measured against InterWeave on 2026-08-26 and dropped — the suite caught everything, at ~1.6h/night and three flags needed to avoid lying"
tier: 2
knowledge_scope: full
distilled_at: "2026-09-17"
origin:
  - agent: user
    host: "develop-qzapp"
    project: interweave
    working_copy: InterWeave
derived_from:
  - 6eb8bdfaf1a9dda6
---

## cargo-mutants was measured against InterWeave on 2026-08-26 and dropped — the suite caught everything, at ~1.6h/night and three flags needed to avoid lying

`cargo-mutants` was evaluated against this workspace on **2026-08-26**
and **dropped**. Do not re-propose it without new evidence.

**What was measured.** Package-scoped sweep of
`interweave-transport-runtime`: 467 mutants, 351 caught, 67 "missed",
49 unviable. Two files then re-run correctly (workspace tests, adequate
timeout): `preauth.rs` + `dedup.rs`, 133 mutants — **126 caught, 0
missed, 0 timeouts**.

**Why dropped:**

- **It found nothing.** Every apparent gap was a scoping or timeout
  artefact. The existing suite caught all 133.
- **Cost:** ~14s/mutant workspace-scoped → ~1.6 hours for one crate.
- **It cannot see this project's actual defect source.** It mutates
  production Rust only. The defects that dominated Stage 6 review lived
  in bash guards under `tools/checks/` and in test files — invisible to
  it. See [[stage6-review-retrospective]].

**Three traps, if anyone tries again.** Each silently produces wrong
results rather than failing:

1. **`--timeout` must be set explicitly, well above the suite** (the
   workspace suite is ~37s). The auto-value derived **20s** from a
   baseline whose test phase measured `0s`, which converts MISSED into
   TIMEOUT — two whole runs were invalidated this way before it was
   noticed.
2. **`--copy-target=false`**, or it copies ~19 GB of build directories
   and dies on `No space left on device`.
3. **`--test-workspace=true`**. Package scoping runs only that crate's
   tests and **fabricates misses** — 11 of 11 apparent gaps in
   `connection_manager.rs` were false.

**A correlation that does NOT hold:** the domain-function ledger
(`tools/checks/domain_fn_exempt.txt`) does *not* predict mutation misses.
It records functions with no PRODUCTION caller; their unit tests still
exercise them, so their mutants are caught. The two tools measure
different things.

*References: stage6-review-retrospective*
