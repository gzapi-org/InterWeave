---
role: "p2p-network-dev"
class: workflow
description: Canonicalizing a lookup key at the write site but not the read site is worse than not canonicalizing at all; find every site before landing the first
tier: 1
knowledge_scope: full
distilled_at: "2026-09-17"
origin:
  - agent: user
    host: "develop-qzapp"
    project: interweave
    working_copy: InterWeave
derived_from:
  - 748fbb96b1bd6c91
---

## Canonicalizing a lookup key at the write site but not the read site is worse than not canonicalizing at all; find every site before landing the first

**A normalization applied to one half of a key pair is a regression, not a
partial fix.**

On InterWeave PR #86 I canonicalized a dial address at `attempt_dial`, so
the admission ticket and the address quarantine keyed the stripped form,
and left `learn_address` storing whatever string arrived. One physical
route then held three keying domains instead of one, and four things broke
that had worked before:

- the scheduler became a per-tick refusal loop, because the candidate
  filter looks up the BOOK string in the quarantine map and no longer
  matched, so a quarantined route was re-offered and re-refused forever;
- `record_permanent_failure`'s `known.remove` missed, so an undialable
  address held one of eight per-peer slots permanently;
- one route occupied two book entries, which is the exact duplication the
  change existed to remove;
- book eviction could see no quarantine, so a full book refused to learn.

None of it was a bypass. The quarantine was honoured throughout. That is
what made it hard to see: every individual check still worked.

**How to apply.** Before changing what a key is computed from, enumerate
every site that WRITES that key and every site that READS it, and change
them together or not at all. Then make the rule unbypassable rather than
repeated: one wrapper that is the only path to the underlying call, plus a
test that fails when a second path appears. Repeating the normalization at
each call site is the same shape as the defect.

A second round found the same class again in the same change: the
settlement path still computed the key its own way, agreeing only by
coincidence of implementation.

Related: [[assertions-that-cannot-fail]], [[anchor-inserts-after-not-before]].

*References: anchor-inserts-after-not-before, assertions-that-cannot-fail*
