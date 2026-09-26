---
role: "p2p-network-dev"
class: workflow
topic: "supply-commits-to-the-callers-change"
description: "the owner's supply process (2026-09-18, gzapp PR #866, broadcast by architect-cto) — a change has ONE owner, the caller, who holds the branch, the PR and the arming; a role producing a piece that would not ship on its own is a supplier…"
tier: 1
knowledge_scope: full
distilled_at: "2026-09-26"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 8a1b5395c3bdc9f3
---

## the owner's supply process (2026-09-18, gzapp PR #866, broadcast by architect-cto) — a change has ONE owner, the caller, who holds the branch, the PR and the arming; a role producing a piece that would not ship on its own is a supplier and pushes commits onto the caller's branch, never a PR of its own; work that ships on its own stays its own PR under the count rule

The owner's rule, in force from gzapp PR #866 and stated for every project (architect-cto's DECISION, GZCoord seq 2387, 2026-09-18):

- **Caller**: the role whose lane holds the change's consuming code, contract or screen. Owns the branch, the PR and the arming.
- **Supplier**: any role producing a piece that would not ship on its own if the caller's change never happened — a migration this PR reads, a check for this feature, copy for these keys. The same login is caller on one task and supplier on the next. **A supplier never opens a PR for supplied work.** Work that ships on its own (a tooling change nobody asked for, a data fix) stays its own PR under [[arming-rule-by-pr-commit-count]].
- **One commit inside one working turn** → pushed onto the caller's branch. The caller's `REQUEST` names the branch and its base sha (`DELIVER-TO`), what is asked (`ACCEPTANCE`: for a check what red and green mean; for a migration the reader and the columns) and `BY` (when the branch is otherwise done). The supplier pushes commits only; the caller does not rebase or force the branch until the `REPLY`.
- **More commits, or an open-ended wait** → the supplier's branch `<host>/<supplier-login>/for/<caller-login>/<what>`, folded by the caller `--no-ff`, unrebased; the PR body states `<sha>..<sha>: <login>`.
- **No caller branch yet** → the caller cuts it first. A hand-off ahead of it carries `DELIVER-TO` (the intended caller role) and `FOLD-BY` (next day by default); past it unclaimed, ping once, then your own PR under the count rule. Owed is a state with a clock.
- **The supplier reviews before hand-off** (for a check, the self-test; for a migration, the idempotency re-run) and records it in the commit trailer `Supplier-Review: <one line>` and the `REPLY`'s `SUPPLIER-REVIEW`. The caller's one blind review covers the range; a finding on a supplier hunk goes to the supplier (the lane by `.agent-fabric/taxonomy.json` paths), fixed on the same branch.
- Work commits of every lane count toward the 8–16 gate; the caller arms; the supplier's `REPLY` is its "at the gate"; a caller does not arm while a `REQUEST` of its own for that PR has no `REPLY`.
- Person-facing copy is never self-authored: the caller drafts en-US, language-culture finalises every locale, each locale holder its own supplier with its own commit.

**Why:** the pull request is the unit of organisational change and a branch is the disposable workspace that builds it; two PRs for one change put the decision and its follow-up under two reviews and two landings.

**How to apply (p2p-network-dev):** when devex-tooling's CI change or architect-cto's contract is the consuming change, I supply a commit to their branch (with `Supplier-Review:`), not a PR. When my transport change needs a check from devex-tooling or a contract clause from architect-cto, I am the caller: cut the branch, send the `REQUEST` with `DELIVER-TO`/`ACCEPTANCE`/`BY`, and do not arm until the `REPLY`. A fix I find in my own lane that ships on its own (PR #90, #91) stays its own PR.

*References: arming-rule-by-pr-commit-count*

*Observed 2026-09-18 (p2p-network-dev)*
