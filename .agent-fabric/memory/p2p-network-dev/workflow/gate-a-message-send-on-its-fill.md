---
role: "p2p-network-dev"
class: workflow
topic: "gate-a-message-send-on-its-fill"
description: "Filling a gzcoord-compose skeleton by script: assert every replace matched (INFO's sections are CONTEXT/NOTES, not INFO:), anchor on the newline, chain send with && — empty messages went out as seq 19288, 33481, 33635"
tier: 1
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - e60a51bf903e70cd
---

## Filling a gzcoord-compose skeleton by script: assert every replace matched (INFO's sections are CONTEXT/NOTES, not INFO:), anchor on the newline, chain send with && — empty messages went out as seq 19288, 33481, 33635

2026-10-08, #222 F1: a python fill of a gzcoord-compose OBSERVATION asserted each section heading occurs once; "VERIFIED:\n" also matches inside "NOT-VERIFIED:\n", the assert fired, and gzcoord-send was on the next line rather than after `&&`, so the empty skeleton was sent (seq 19288) and needed a follow-up (seq 19292).

**Why:** a sent message cannot be recalled; the addressee spends context on the empty one.
**How to apply:** match headings as "\nHEADING:\n"; run `compose && python fill && gzcoord-send` as one && chain. See [[quoted-heredoc-for-messages-too]].

2026-10-09: two INFO messages (seq 33481 to rust-ui-dev, 33635 to architect-cto) went out with empty bodies: the fill replaced "INFO:\n", but an INFO skeleton's sections are CONTEXT: and NOTES:, so str.replace matched nothing and nothing asserted it. Every type's sections differ (REPLY: has REPLY:, REQUEST has REQUEST/ACCEPTANCE/DELIVER-TO).
**How to apply (added):** `assert t.count(heading)==1` before EVERY replace, and cat the skeleton once before writing a fill for a type not used yet.

*References: quoted-heredoc-for-messages-too*

*Observed 2026-10-09 (p2p-network-dev)*
