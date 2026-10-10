---
role: "p2p-network-dev"
class: threads
topic: "pr179-vendoring-landed"
description: "#179 (ADR-0054 vendoring, provenance check, two carried fixes) merged 97d83830 on 2026-10-04; what it carries"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - be832c62b7784362
---

## #179 (ADR-0054 vendoring, provenance check, two carried fixes) merged 97d83830 on 2026-10-04; what it carries

#179 merged as 97d83830 on 2026-10-04, on the owner's word ("arm"). It holds:
- third_party/wasm-bindgen-futures, with 3 pins relaxed;
- futures-timer 3.0.4, with the wasm-bindgen family at 0.2.129 and js-sys at 0.3.106;
- check_vendored_advisories now naming each crate it checked;
- check_vendored_provenance.py, which compares each tarball's sha256 with the record, reverse-applies the patch, compares every byte, and requires a non-empty LICENSE when the tarball has no licence file;
- the wire-audit negative test;
- a failed ComposedRuntime::start shutting its substrate down.

Supplied commits folded into it: devex-tooling's ci.yml step, and architect-cto's ADR-0054 count fix (11 files) and ADR-0051 amendment.

Carried risk (from the final re-review): the licence-file test matches LICENSE*/LICENCE*/COPYING* names at ANY depth in the tarball. A tarball whose only licence file sits in a subdirectory would not require a top-level LICENSE. No current tree has this shape. The fix: restrict the test to top-level names (no "/"), plus a self-test case.

Lesson: my own counts were wrong twice (15, then 10; it is 11). Count with `git ls-tree` or `tar tzf`, never from memory.

*Observed 2026-10-04 (p2p-network-dev)*
