---
role: "p2p-network-dev"
class: solution
description: "Stage 8 (endpoint directory) shipped in 2 PRs, ~22 automated-review findings over 15 rounds; the source-spoofing P1, the vacuous-test lesson, and what the review chained on"
tier: 2
knowledge_scope: full
distilled_at: "2026-09-17"
origin:
  - agent: user
    host: "develop-qzapp"
    project: interweave
    working_copy: InterWeave
derived_from:
  - 3eab29ed17a797dc
---

## Stage 8 (endpoint directory) shipped in 2 PRs, ~22 automated-review findings over 15 rounds; the source-spoofing P1, the vacuous-test lesson, and what the review chained on

Stage 8 (`/interweave/endpoints/1.0.0` + the inherited source-endpoint
obligation) landed 2026-08-28 as **PR #54** (implementation, 12 review
rounds, ~19 findings) and **PR #55** (the close, 3 rounds, 3 findings).
Every finding was real; none was disputed.

**Why so many rounds — the patterns, so the next protocol-add expects them:**

- **New wire protocol at a trust boundary → lifecycle parity is the axis.**
  Most PR-#54 findings were the directory path missing a defense the
  DIRECT path already had: unbounded outbound queries, shutdown
  settlement blind to directory work, no drain refusal, trust served
  before the cache, a refusal that skipped the in-flight bound, a
  response accepted after mid-flight revocation. When you mirror an
  existing subsystem, diff it against the original's whole lifecycle
  (admission, bounds, shutdown, reload, revocation), not just its happy
  path.
- **The real P1 was the one I half-fixed first.** The inherited obligation
  was "bind the source to the caller's lease." I first overwrote the
  frame's `source_endpoint` from a `session: String` — but a string is
  forgeable, so any handle holder could name another session. The fix was
  the unforgeable capability: `send_direct(&EndpointLease)`, epoch
  verified against the live lease (`holds_lease`). `ENDPOINTS.md`'s
  "callers cannot spoof another local endpoint" is enforced, not
  documented. Cost ~85 test call-site migrations. **A "derive from X"
  requirement is only closed when X is unforgeable.**
- **Config the schema documents but nothing reads.** `config.schema.yaml`
  had `max_queries_per_minute_per_peer`, `max_inflight_queries`,
  `cache_ttl` under `directory:`; profile-config parsed none, and
  `deny_unknown_fields` REJECTED any profile using them. Wiring one
  exposed the reload path, which exposed the next unparsed field — a
  3-round chain. When adding a config-driven feature, grep the schema for
  the whole block, not the one field the ticket names.

**The sharpest lesson (PR #55, the close): a vacuous test.** My
end-to-end hostile-directory test dropped the request event's
`ResponseChannel`, so the malformed frames never transmitted — and an
empty response ALSO decodes as `ProtocolViolation`, so all three
iterations passed green while proving nothing. Exactly CLAUDE.md §4's
"a test written from the same belief as the code agrees with it for
free." The fix wasn't the dispatch line; it was **mutation-checking both
failure layers** (validate_response duplicate, codec `Io(InvalidData)`)
to prove the test was load-bearing. Over-the-wire negative tests are the
easiest to make vacuous — always break the guard and watch THAT test go
red, and prefer an assertion only the real bytes can satisfy.

**Ledger mechanics reconfirmed** (see [[claude-md-skill-split-after-stage-7]]
for the tooling): a `Type::method` with a common name (`new`, `len`,
`with_defaults`, `lease`) trips the heuristic as "referred", so its
exemption must be DROPPED not dated; method-position calls the heuristic
can't attribute become `call <expr>` entries; a `stage-N` deadline fires
the moment the open stage passes N. `TopicKey::as_bytes` carried a note
demanding delete-or-justify at Stage 9 — deleted, the frozen vector now
compares `wire_string` (Rust hex) to the fixture's Python-computed
sha256, a real cross-check.

See [[stage6-review-retrospective]] for the earlier cycle; the shape
(contract-vs-code mismatch, fix-induced regressions) repeats.

*References: claude-md-skill-split-after-stage-7, stage6-review-retrospective*
