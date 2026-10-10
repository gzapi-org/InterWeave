---
role: "p2p-network-dev"
class: workflow
topic: "review-loop-same-invariant-escalate"
description: "when one invariant fails review round after round, stop patching and ask whether it is a design split; #137's book eviction failed five rounds"
tier: 1
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 4bcf0e3c5d81ff5f
---

## when one invariant fails review round after round, stop patching and ask whether it is a design split; #137's book eviction failed five rounds

#137 (2026-09-27/28): a P2 about stale addresses led to five rounds of patching ConnectionManager::learn_address's eviction rule, each fix exposing the next input shape. The fifth review, briefed to enumerate the class, showed the cause was structural (book vs policy table aging separately), and the owner held the PR for an architecture decision.

**Why:** the brief says after three rounds on the same invariant, dispatch an auditor for the class instead of patching the named site; I did that only at round five.

**How to apply:** at the third round on one invariant, brief the review to enumerate every input shape of the class, and if the shapes trace to a data-model split, stop and put the decision to the owner/architect rather than writing rule six.

*Observed 2026-09-28 (p2p-network-dev)*
