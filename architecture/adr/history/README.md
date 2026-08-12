# ADR amendment history

One file per amended ADR: `NNNN-amendments.md`, matching the ADR's number.

**ADR bodies read current** (ADR-0048). An ADR states the decision as it stands today, with every amendment folded into the section it belongs to, so this directory is for research — how a decision evolved, what the prior wording was, why it changed. Reading an ADR body plus its digest entry is complete without opening anything here.

## Format

Each amendment appends one dated note:

```markdown
### Amendment YYYY-MM-DD — short title

What changed, and why. Quote the prior wording where the old text still
matters to someone holding a stale citation.
```

Every note has a counterpart row in its ADR's `## Amendments` end-matter table, date-keyed and carrying the same title verbatim, so a citation of the form "ADR-0031 Amendment 2026-09-01" resolves from the ADR to the row to the note. `tools/checks/validate_adr_index.sh` compares the two as multisets and fails on a note without a row, or a row without a note.

The date alone is **not** an identity — a note is identified by its (date, title) pair, so same-day amendments with distinct titles need no disambiguator. A `(ii)` / `(iii)` suffix is added only where two headings in one file would otherwise be byte-identical; `tools/checks/scan_semantic_collisions.sh` fails on that case, because a citation could then resolve to either.

An ADR with no amendments has no file here and no `## Amendments` section. Both appear with its first amendment.

## What is not here

There is no changelog and no corpus version counter. Beyond these notes, the mechanical change record is the git commit.
