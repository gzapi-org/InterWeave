# <Decision, stated as a claim>

**Status:** Accepted

<!--
FILE:   architecture/adr/NNNN-lowercase-kebab-slug.md
        Four-digit number, next free after the highest in README.md.

TITLE:  the decision itself, not the topic and not the number — "Deny-by-default
        static PeerId trust policy", not "Trust" and not "ADR-0012 — Trust".
        The number is in the filename; repeating it in the heading is noise.

STATUS: `**Status:** Accepted` is the common case. The line carries free-form
        prose after the status word, and that prose is where supersession is
        recorded, e.g.

          **Status:** Superseded by ADR-0035
          **Status:** Accepted; supersedes ADR-0024
          **Status:** Accepted integration/security design; rollout clauses
                      superseded by ADR-0034
          **Status:** Superseded in part by ADR-0034; cache/mDNS/static
                      provider roles remain accepted

        Mark status truthfully. Do not write `Accepted` on a decision the
        owner has not accepted.

The eight sections below are mandatory and appear in this order. A platform-
or scope-specific extra section may be added when it genuinely belongs to the
decision; extras never replace a mandatory section. `## Amendments` is the one
permitted end-matter section and is always last (see the amendment note at the
bottom of this file).

Delete this comment block when authoring.
-->

## Context

What forces the decision. The constraint, the conflict, or the gap that exists today — enough that a reader who has never seen the discussion understands why a decision was needed. State assumptions explicitly rather than inventing facts; an unsupplied constraint is named as an assumption, not guessed at.

## Decision

What is decided, in the present tense, as binding text. This section is the decision: it must be readable on its own.

Use a bulleted or numbered list when the decision has separable rules — numbered items are citable, and **their numbers are permanent** (ADR-0048). A withdrawn item keeps its number as a one-line tombstone rather than being removed or reused.

Name the wire-visible consequences precisely: protocol names, identifiers, and state names belong here. Numeric limits and hash vectors belong in `architecture/contracts/`, `architecture/transport/`, and `fixtures/` — cite them, do not restate them, unless this ADR is the thing that sets them.

## Alternatives considered

What else was on the table and why it lost. One dense paragraph is normal; the point is to stop a future reader from re-proposing a rejected option without new information.

## Consequences

What follows from the decision — what becomes possible, what becomes harder, what other components must now do. Include the costs, not only the benefits.

## Security implications

What this decision does to the threat model: what an attacker can and cannot do under it, which invariant it rests on, and what residual risk it accepts. "None directly" is a legitimate answer when the decision is genuinely security-neutral — say it and say why, rather than leaving the section empty.

## Operational implications

What an operator configures, observes, or must understand. Failure modes visible in production, and what degrades versus what stops.

## Implementation implications

What has to be built, and where it lives in the layout of ADR-0045. Call out anything that must be inert until an earlier stage gate passes (ADR-0046).

## Revisit conditions

The concrete conditions under which this decision should be reopened. Not "if requirements change" — the specific measurement, scale, platform behaviour, or product requirement that would invalidate the reasoning above.

<!--
AFTER WRITING — propagation in the same commit series (ADR-0048):

  1. add the row to architecture/adr/README.md
  2. add the entry to architecture/adr/ADR-DIGEST.md, in a cluster, plus a
     keyword-table row if it introduces a searchable topic
  3. update any contract/transport/discovery/client spec that inherits the rule
  4. update fixtures/ only if a frozen vector intentionally changes
  5. put the rationale in the COMMIT BODY — there is no changelog
  6. for an amendment, also do the three-part record below

  bash tools/checks/validate_adr_index.sh   # enforces 1, 2, 6, and this template

AMENDING an existing ADR is a different act with a three-part record:

  1. edit the body IN PLACE, folding the change into the section it qualifies
     (never append explanatory end-matter — the body must read current)
  2. append a dated note to architecture/adr/history/NNNN-amendments.md:
       ### Amendment YYYY-MM-DD — short title
     saying what changed and why, quoting prior wording where a stale
     citation would otherwise mislead
  3. add the row to that ADR's `## Amendments` end-matter table — the last
     section in the file — carrying the SAME date and title verbatim:
       | Date | Amendment | Effect |

  The (date, title) pair is the identity, so same-day amendments with
  distinct titles need no disambiguator; a (ii)/(iii) suffix exists only to
  break byte-identical headings. The validator compares table rows and
  history headings as multisets.

  A change of SUBSTANCE is not an amendment — it is a new superseding ADR.
  Test: would a reader who followed the old text now be WRONG? Then supersede.
-->
