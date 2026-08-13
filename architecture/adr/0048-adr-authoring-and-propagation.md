# ADR authoring, amendment, and propagation

**Status:** Accepted

## Context

The ADR set is the source of architectural intent and has grown to a size where reading it linearly is no longer the cheapest correct way to answer a question. Two separate problems follow from that.

The first is **loading**. A contributor or automated session that must "read the ADRs" before touching a subsystem either reads all of them, which is expensive and mostly irrelevant, or guesses from filenames, which silently misses the ADR that actually governs the change. Numbered filenames sort by decision date, not by topic, so the corpus offers no topical entry point.

The second is **amendment**. Decisions evolve: a later ADR narrows an earlier one, a review closes a gap, a spike disproves an assumption. Without a stated model this is recorded ad hoc — sometimes as a new ADR, sometimes as a silent edit, sometimes as a trailing note — and a reader cannot tell whether an ADR body describes what is true now or what was true when it was written. Two ADRs (0015, 0037) carried trailing `## Android amendment` sections, one convention among several and never written down.

Nothing here is implemented yet, so this is the cheapest moment to fix the model. The choice is between letting git carry the entire history and keeping a curated record beside the documents. Git records every change mechanically but shows a typo fix and a substantive narrowing at identical weight, explains *why* only as well as each commit body happens to, and does not travel when the specification tree is handed off as an archive — which this repository does. A curated record costs more to write and can drift from the decision text, so the drift has to be checked rather than trusted.

## Decision

### The ADR body is the current decision text

An ADR's body states the decision **as it currently stands**. Amending an ADR **edits the body in place**, folding the change into the section it belongs to, so the file always reads current and a reader never reconstructs the present from an original plus a stack of notes.

Two stability rules keep the citation web intact:

- **Numbered decision items are permanent identifiers.** Where a Decision section uses a numbered list, other documents cite those numbers. A number is never renumbered or reused: a withdrawn item keeps its number as a one-line tombstone (`4. — withdrawn, Amendment 2026-09-01`), and new items take the next free number.
- **A re-decision supersedes; it does not amend.** Changing what a decision *is* means a new ADR that supersedes the old one, whose `**Status:**` line then points at the successor — exactly as ADR-0030 superseded ADR-0016's routing semantics and ADR-0035 superseded ADR-0024. An amendment narrows scope, records a consequence, or folds in a refinement the decision already implied. The test is whether a reader who followed the old text would now be **wrong**: if yes, supersede.

Where an earlier convention put a platform note in a trailing `## Android amendment` section (ADR-0015, ADR-0037), that text is folded into the Decision it qualifies and recorded as an amendment under the model below.

### Every amendment is recorded three ways, in one commit series

1. the **in-place body edit**;
2. a dated note appended to `architecture/adr/history/NNNN-amendments.md`, heading grammar `### Amendment YYYY-MM-DD — title`, saying what changed and why, and quoting the prior wording where the old text still matters to someone holding a stale citation;
3. a row in the ADR's `## Amendments` end-matter table (`| Date | Amendment | Effect |`) — always the last top-level section — date-keyed and carrying the history note's title verbatim.

A citation of the form "ADR-0031 Amendment 2026-09-01" therefore resolves from the ADR to the row and, through it, to the note.

**The date alone is not an identity.** A row is identified by its (date, title) pair, so same-day amendments with distinct titles are unambiguous and need no disambiguator. A `(ii)` / `(iii)` suffix is added **only** where two headings in one file would otherwise be byte-identical, which is precisely the ambiguity `tools/checks/scan_semantic_collisions.sh` fails on. `tools/checks/validate_adr_index.sh` compares the table's date keys against the history headings as multisets, so a row without a counterpart — or a note without a row — fails.

A new ADR has no `## Amendments` section and no history file. Both appear with its first amendment.

### There is no changelog and no version counter

The corpus has no version number and no changelog. Beyond the three-way record above, the change record is the **git commit**: the subject names the change, and the body carries reasoning that does not belong in either the decision text or the amendment note. The history file is the curated *why*, written at the time and travelling with the documents; git remains the complete mechanical record.

### Propagation is part of the change, not follow-up

An ADR change is never the ADR file alone. In the same commit series:

1. add or update the row in `architecture/adr/README.md`;
2. add or update the entry in `architecture/adr/ADR-DIGEST.md`, placed in a cluster, and add a keyword-table row when the ADR introduces a topic a reader would search for;
3. update any contract, transport, discovery, or client specification whose text inherits the changed rule;
4. update `fixtures/` if and only if the decision intentionally changes a frozen vector;
5. for an amendment, add the history note and the end-matter row described above.

`tools/checks/validate_adr_index.sh` enforces the mechanical part — items 1 and 2, the template every ADR follows, and the amendment record's three-way consistency — and is part of the repository-wide verification list in `CLAUDE.md` §9.

### The digest is the default entry point, and it is non-normative

`ADR-DIGEST.md` carries one current-state entry per ADR plus a keyword → ADR lookup table. A reader answers a question from the digest and opens the full ADR only when the change touches that ADR's substance. The digest decides nothing: on any discrepancy the ADR wins and the digest is what gets corrected. Normative constants are never read from it.

## Alternatives considered

**In-place edits with git as the sole history** — no note, no row, `git log -p` on the file. Genuinely cheaper: one edit, no drift, nothing to check. Rejected because a diff records what changed and never why, leaving the rationale hostage to commit-message discipline; because a commit SHA is not a citation a document can carry; and because the specification tree is handed off as an archive, which git history does not accompany. Its revisit condition is written above: if the notes stop being read, this is what to collapse back to.

**Append-only bodies with a trailing amendment log** — the reader reconstructs the present from an original plus N notes, which is the failure this ADR exists to prevent.

**A superseding ADR for every change** — number inflation, and the citation web breaks continuously because the governing ADR keeps moving.

**A corpus changelog with a version counter** — a second mechanical change record beside git, guaranteed to drift from it, and no better at explaining *why* than a commit body.

**A generated machine index as the propagation target** — a third artifact to keep in sync when the human-readable index and digest already carry the same facts and are the things people actually read.

**No digest at all, relying on filename search** — the status quo this ADR replaces: filenames sort by date, not topic, so the governing ADR is missed silently.

## Consequences

An ADR body can be trusted as current without reading its history, which is what makes topical loading safe: the digest entry summarises a body that is true now. History research becomes an explicit act — open `architecture/adr/history/` — rather than the default reading experience.

Amending costs three edits plus propagation rather than one, permanently, and introduces a class of drift that did not previously exist: body, note, and row can disagree. That is why both the multiset key check and the byte-identical-heading scan exist; without them the record would be trusted precisely where it had gone wrong. The benefit bought is a *why* written at the time by whoever knew it, a stable human citation that survives rebases and needs no repository access, and history that ships with the archive.

The digest becomes a maintained artifact with a real staleness risk. The mitigation is mechanical presence-checking plus the rule that the ADR always wins; a stale entry is a bug in the digest, never an authority conflict.

## Security implications

None directly. Indirectly, a reader who finds the governing rule cheaply is likelier to honour it: the invariants most often lost to a partial reading are exactly the security-relevant ones — endpoint policy narrowing but never widening, `Ignore` versus `Reject` for an unauthorized publisher, infrastructure authorization not granting data-plane trust.

The digest must never become a place where a normative constant is read, because a drifted limit or vector there would be a security regression in a file explicitly marked non-normative. Hence the standing rule that limits and vectors come from contracts and `fixtures/`.

## Operational implications

Contributors and automated sessions load the digest plus the repository-root `CLAUDE.md` and open full ADRs on demand. The `adr-lookup` and `adr-authoring` skills carry the reading and writing procedures respectively, so neither is duplicated into `CLAUDE.md`.

## Implementation implications

`architecture/adr/ADR-DIGEST.md`, `architecture/adr/ADR-TEMPLATE.md`, and `architecture/adr/history/` are documentation artifacts requiring no production code. `tools/checks/validate_adr_index.sh` is a repository check with a self-test, run before pushing and listed in the repository-wide verification list; it enforces the template, both propagation directions, and the amendment record's three-way consistency.

## Revisit conditions

Revisit if the ADR set grows past the point where one digest file is itself expensive to load, in which case per-cluster digests become the natural split; if amendment traffic makes the three-way record the dominant authoring cost without the notes being read, which would argue for collapsing back to git as the sole record; or if a generated index becomes necessary to serve tooling that cannot parse the human-readable ones.
