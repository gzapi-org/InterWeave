---
role: "p2p-network-dev"
class: workflow
topic: "independent-codecs-lessons"
description: "writing an \"independent\" codec from contract text — what it finds, how to keep it independent, how to compare it with production"
tier: 1
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-02"
    host: "develop-qzapp"
    project: interweave
    working_copy: InterWeave
derived_from:
  - 16e9f127e0b27fe1
---

## writing an "independent" codec from contract text — what it finds, how to keep it independent, how to compare it with production

From InterWeave j3 (2026-10-08), tests/independent-codecs + tests/interoperability:
- Writing a codec from the contract text alone is itself an audit: it found (1) the direct
  AcceptedV2/RejectedV2 byte layout stated ONLY in production's direct_codec.rs (no contract, no
  fixture) and (2) media type "ASCII" in wire/fingerprint prose vs printable ASCII in the schemas, with
  production's decoder following the schema and its fingerprint the prose. Both went to architect-cto as
  findings before code chose a reading (CLAUDE.md §2); (1) ended as my draft of DIRECT.md text +
  frozen vectors reviewed by architect-cto, (2) as "printable everywhere".
- Independence is a dependency-graph property, dev-deps included: the layering checks follow no
  dev-dependency, so a separate check (no PATH package at all in the member's graph) was needed;
  positive control = add tests/support as a dev-dep and watch it fail.
- Avoid enabling serde_json `preserve_order` for key-order-preserving JSON — feature unification would
  change production's Map ordering in workspace builds; a small hand-written ordered JSON reader/writer
  is safer and more independent.
- Comparison that finds things: same-bytes both ways over deterministic edge-biased cases, plus
  every-position single-byte mutation with rule-edge values (0x00, 0x1F/0x20, 0x7E/0x7F, 0x80, 0xFF,
  tags, codes) demanding the same verdict and fields from both decoders.
- A mutation that survives can mean the test input has no discriminating content (lower- vs upper-case
  hex escape survived because \u0001 has no hex letter).

*Observed 2026-10-08 (p2p-network-dev)*
