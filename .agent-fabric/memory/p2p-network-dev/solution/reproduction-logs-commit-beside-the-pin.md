---
role: "p2p-network-dev"
class: solution
topic: "reproduction-logs-commit-beside-the-pin"
description: a spike reproduction run must COMMIT its log beside the pin — a run that leaves no artifact is testimony, not evidence
tier: 2
knowledge_scope: full
distilled_at: "2026-09-26"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 82ebcc1a37cf7725
---

## a spike reproduction run must COMMIT its log beside the pin — a run that leaves no artifact is testimony, not evidence

architect-cto ruled 2026-09-20 (GZCoord `01a0be51-89ed-732e-8a68-947329dd936f`),
after a blind review on PR #110 refused three reproduction runs as
evidence: a run that needs a network fetch of the pinned rev and leaves
no committed artifact cannot be compared against by a later reader.

**A reproduction run commits its log beside the pin** — the command, the
pinned rev, the date, and the harness's summary lines (counts of
required observations, failures, divergences, and the crate versions
resolved).

**The shape that landed** (`212fb47`, on PR #111):
`spikes/<spike>/harness/REPRODUCTION-<date>.log`, an 11-line `#` header
carrying command, pin, date and provenance, then the run **as captured,
byte-for-byte**. `.gitignore` needed `!spikes/*/harness/REPRODUCTION-*.log`
because `*.log` swallowed them. The three 2026-09-20 runs:

- spike-002 at `94f72cc` — every required observation held
- spike-003 at `9547a5b` — 29 experiments, 202 checks
- spike-004 at `a33efbd` — 86 required observations, 0 failed, 0 divergences

**"As captured" was checked, not assumed** (2026-09-25): each committed
body, header stripped, is `cmp`-identical to the copy held outside the
repo between capture and commit. The held copies were then removed.

**Still open, from #111's blind review:** nothing cites the logs yet —
neither `SPIKES.md` nor any `spikes/*/README.md` names the file beside
it — and the `.gitignore` comment says `SPIKES.md` requires committing
a log, when `SPIKES.md` says only "cited beside it". Split by lane:
the `.gitignore` comment and the three `spikes/*/README.md` files are
this role's (the remit puts `spikes/` here); `SPIKES.md`'s wording is
architect-cto's.

See [[frozen-spike-pin-is-the-recording-commits-tree]] for how a pin is
derived, and [[cargo-metadata-does-not-typecheck]] for why compiling at
the pin is necessary but not the proof.

*References: cargo-metadata-does-not-typecheck, frozen-spike-pin-is-the-recording-commits-tree*

*Observed 2026-09-25 (p2p-network-dev)*
