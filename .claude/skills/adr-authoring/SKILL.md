---
name: adr-authoring
description: Writing a new InterWeave ADR, amending an existing one, or propagating an ADR change through the index and digest. Use whenever you add an architecture decision, change what an accepted one says, or discover that a spike or implementation disproved an accepted assumption. For READING/navigating existing ADRs use adr-lookup instead. Rule of thumb: an ADR change that does not touch README.md and ADR-DIGEST.md in the same commit series is incomplete, and the validator will say so.
---

# Writing and amending ADRs

The model is ADR-0048; the section structure is [`architecture/adr/ADR-TEMPLATE.md`](../../../architecture/adr/ADR-TEMPLATE.md). This skill is the procedure.

## Before writing anything: is it an ADR at all?

- A **decision** with alternatives that were rejected → ADR.
- An explanation of how something already decided works → a document under `architecture/docs/`, not an ADR.
- A normative wire format, limit, or vector → a contract under `architecture/contracts/` or `architecture/transport/`. The ADR decides *that* there is a limit and why; the contract states its value.
- A change to what an accepted ADR says → the amendment path below, or a superseding ADR.

## Writing a new ADR

**Number and name.** `architecture/adr/NNNN-lowercase-kebab.md`, four digits, next free after the highest in `README.md`. Check `git fetch && git log origin/main` first — another session may have taken the number, and two branches minting the same one merge textually clean. `tools/checks/scan_semantic_collisions.sh` catches it if you do not.

**Title is the decision, not the topic.** "Deny-by-default static PeerId trust policy", not "Trust", and never "ADR-0012 — Trust": the number is in the filename.

**Structure.** The eight sections of the template, in order, all mandatory. An extra scope-specific section is allowed when it genuinely belongs to the decision.

**Status honesty.** Write `Accepted` only for a decision the owner has accepted. The line takes free-form prose after the status word, and that prose is where supersession is recorded.

**No invented facts.** If a constraint or trade-off was not supplied, do not guess it. State it as an explicit assumption inside the ADR and surface it in your reply so it can be confirmed or corrected.

**No external-project citations.** Do not name an unrelated external project, in the ADR or in the commit message (`CLAUDE.md` §7). InterWeave ADRs stand on their own.

**Write the security section honestly.** "None directly" is a legitimate answer — say it and say why. An empty section is not.

## Amending an existing ADR

An amendment is a **three-part record in one commit series**:

1. **Edit the body in place**, folding the change into the section it qualifies. Never append explanatory end-matter — the body must read current, which is what makes digest-first loading safe.
2. **Append a dated note** to `architecture/adr/history/NNNN-amendments.md`:
   ```markdown
   ### Amendment YYYY-MM-DD — short title

   What changed and why. Quote the prior wording where a stale citation
   would otherwise mislead.
   ```
3. **Add the row** to the ADR's `## Amendments` end-matter table — the last section in the file — carrying the same date and title **verbatim**:
   ```markdown
   | Date | Amendment | Effect |
   |---|---|---|
   | 2026-09-01 | Short title | What a reader now does differently |
   ```

The (date, title) pair is the identity, so same-day amendments with distinct titles need no disambiguator; a `(ii)` / `(iii)` suffix exists only to break byte-identical headings. `validate_adr_index.sh` compares rows and headings as multisets — a row without a note, or a note without a row, fails.

**Numbered decision items are permanent.** Other documents cite them. Never renumber or reuse: a withdrawn item becomes a one-line tombstone, new items take the next free number.

## Amend or supersede?

The test: **would a reader who followed the old text now be wrong?**

- Yes → **supersede.** New ADR; the old one's `**Status:**` points at it. This is what ADR-0030 did to ADR-0016's routing semantics and ADR-0035 did to ADR-0024.
- No → **amend.** Narrowing scope, recording a consequence, folding in a refinement the decision already implied.

Partial supersession is normal and is recorded in the Status prose: ADR-0008's rollout was superseded by ADR-0034 while its provider roles stand; ADR-0009's default-disabled clause went the same way while all its security semantics remain in force.

## Propagation — the part that gets skipped

In the **same commit series**:

1. row in `architecture/adr/README.md`;
2. entry in `architecture/adr/ADR-DIGEST.md`, placed in a cluster, plus a keyword-table row if it introduces a topic someone would search for;
3. any contract, transport, discovery, or client specification whose text inherits the changed rule;
4. `fixtures/` **only** if the decision intentionally changes a frozen vector — that is a protocol change, not a fixup;
5. the rationale in the **commit body**.

Then:

```
bash tools/checks/validate_adr_index.sh
bash tools/checks/scan_semantic_collisions.sh
```

## When a spike or implementation disproves an ADR

Do not code around it and do not leave the ADR standing. Update the ADR or contract in the **same change** that acts on the new behaviour (`CLAUDE.md` §2) — an accepted document that the code contradicts is worse than no document, because the next reader trusts it.
