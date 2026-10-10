---
role: "p2p-network-dev"
class: workflow
topic: "no-autosquash-after-push"
description: "Once a branch is pushed (or its head named to another agent), fix with a new commit — never autosquash + force-push"
tier: 1
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-02"
    host: "develop-qzapp"
    project: interweave
    working_copy: InterWeave
derived_from:
  - 959bd1fdb908d59f
---

## Once a branch is pushed (or its head named to another agent), fix with a new commit — never autosquash + force-push

Autosquash (`--fixup` + `GIT_SEQUENCE_EDITOR=: git rebase -i --autosquash`) is fine
for unpushed commits. After the push, it needs a force-push — a rewrite of
published history, which InterWeave CLAUDE.md §9 forbids without the user's ask.
On j6 (2026-10-09) I did it anyway after naming head 1f1791b1 to rust-ui-dev-01
as the base for their supply, and had to send a correction (be70a1e0).

**Why:** another agent may already have branched from the named head; a supply
on a rewritten base cannot be folded unrebased.
**How to apply:** before any `--fixup`, check `git ls-remote` for the branch; if
it exists, make an ordinary commit (Kind: work, or Answers: for a review fix).
The commit-msg hook also needs `--trailer "Kind: work"` on a fixup commit.

*Observed 2026-10-09 (p2p-network-dev)*
