---
role: "p2p-network-dev"
class: solution
description: "Stage 6 took 13 review rounds and 37 findings; 46% were contract-code mismatches and 35% were caused by the previous round's fix"
tier: 2
knowledge_scope: full
distilled_at: "2026-09-17"
origin:
  - agent: user
    host: "develop-qzapp"
    project: interweave
    working_copy: InterWeave
derived_from:
  - a25ce7795e21d3fb
---

## Stage 6 took 13 review rounds and 37 findings; 46% were contract-code mismatches and 35% were caused by the previous round's fix

PR #38 (Stage 6, direct v2) took **13 automated review rounds and 37
findings**. Categorised:

- **~17 (46%) contract–code mismatches.** A normative document named an
  error code, required field or limit and the code did otherwise:
  `CapabilityDenied` vs `ENDPOINTS.md` step 3's `UnauthorizedPeer`;
  `PeerUnreachable` vs `DIRECT.md:94`'s `PeerUnknown`; `received_at`
  missing though **required** by `message-received.schema.json`;
  `too_large` for frames that were `malformed`; the queue-depth,
  endpoint-count and in-flight rows of `resource-limits.md` unenforced.
- **~13 (35%) caused by the previous round's fix.** Adding `received_at`
  created the wall-clock bug; the inbound shutdown grace created the
  counter mismatch; the delivery-slack fix created the listener-slack
  and notification bugs.
- **5 were one invariant** — outbox capacity — found five separate times.

Two domain functions were fully implemented, fully unit-tested and had
**zero production callers**: `EndpointRegistry::authorize_outbound` and
`FrameError::to_wire`.

**Why:** the repo checks schema SHAPE (`validate_contracts.py`) but
nothing checks specified BEHAVIOUR, so every error-code rule is enforced
only by whoever last read the prose.

**How to apply:** agreed follow-up after #38 merges, in this order.

1. **Contract conformance matrix** — table-driven tests asserting every
   `(condition -> error code)` pair in `ENDPOINTS.md` outbound steps 1-8,
   `DIRECT.md`, `TRANSPORT.md`, plus required schema fields. Targets the
   46%. Same style as the existing `Refusal::to_wire` totality test.
2. **Uncalled-domain-function check** — a tree check failing on `pub fn`
   in `crates/api/*` and `crates/transport/runtime/*` with no non-test
   caller. Twenty lines, would have caught both functions above.

Also learned: when a finding names one wrong arm, fix the WHOLE contract
table, not the reported line — most of the 35% was the rest of a match
block left untouched. See [[yamux-silent-downgrade]] for the other case
where a check could not see what it claimed to cover.

*References: yamux-silent-downgrade*
