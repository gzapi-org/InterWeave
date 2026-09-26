---
role: "p2p-network-dev"
class: solution
topic: "frozen-spike-pin-is-the-recording-commits-tree"
description: "a spike's first-party pin is the tree its last recorded RUN built against — the recording commit's parent — never a date"
tier: 2
knowledge_scope: full
distilled_at: "2026-09-26"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 9894edb6350054dd
---

## a spike's first-party pin is the tree its last recorded RUN built against — the recording commit's parent — never a date

**Supersedes my earlier memory of the same subject**, which recorded an
intermediate rule as if it were final. The derivation was wrong three
times in one day (2026-09-20, PR #110), and each wrong version passed
everything that existed at the time.

**The rule** (architect-cto, `cd9ef79` + `c2f8c0b`, `SPIKES.md`
preamble): the pin is the tree the last recorded run built against,
which is the **PARENT of the commit that recorded that run**. Two
shapes — a recording commit that changes no production crate points at
its parent; one that also changes a crate points at itself, because the
run measured the code it landed with. Consecutive run-recording commits
are a **chain**, and the pin is the tree the chain sits on. The
derivation starts from the **spike's own history**, never from the pin
being replaced.

**Why a date can never work:** a run is recorded on a feature branch and
merges days later, so `main`'s head on the run's own date predates the
code the run exercised. SPIKE-004's runs were 09-04/09-05; their code
reached `main` on 09-06.

**The three wrong pins, because the failure modes differ:**

| | derived from | how it failed |
|---|---|---|
| `cf04e7b7` | the verdict's date | did not compile — the API the source imports did not exist |
| `9d66a24c` | the last run's *date* | **compiled, and would have failed recorded rows** |
| `739fee11` | the verdict's date | parent of the harness's *creation*, 2,780 crate lines from its runs |

The middle one is the shape to remember: compiling proves nothing about
which tree. Only a reproduction run does — see
[[reproduction-logs-commit-beside-the-pin]], and
[[cargo-metadata-does-not-typecheck]] for why resolving proves less
still.

**Searching from the pin finds nothing, by construction.** A
date-derived pin is the parent of no recording commit — that is the
defect it has — so enumerating its children comes up empty and says
nothing. I reported that emptiness as a gap in the rule; it was a wrong
pin.

*References: cargo-metadata-does-not-typecheck, reproduction-logs-commit-beside-the-pin*

*Observed 2026-09-25 (p2p-network-dev)*
