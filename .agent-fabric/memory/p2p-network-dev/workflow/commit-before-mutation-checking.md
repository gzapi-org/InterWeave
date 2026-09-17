---
role: "p2p-network-dev"
class: workflow
description: "Undoing a planted mutation with git checkout -- reverts the whole file to HEAD and silently destroys the uncommitted fix you were testing; commit first, or restore from a scratch copy"
tier: 1
knowledge_scope: full
distilled_at: "2026-09-17"
origin:
  - agent: user
    host: "develop-qzapp"
    project: interweave
    working_copy: InterWeave
derived_from:
  - ab9fc923693d5d89
---

## Undoing a planted mutation with git checkout -- reverts the whole file to HEAD and silently destroys the uncommitted fix you were testing; commit first, or restore from a scratch copy

**Mutation-check a fix only after committing it, or restore the mutation
from a scratch copy — never with `git checkout --`.**

On InterWeave PR #86 I fixed a guard, ran the tests, planted a mutation,
watched the guard fail correctly, and then ran:

```
git checkout -- crates/transport/libp2p/src/runtime/mod.rs \
                crates/transport/libp2p/src/runtime/dialing.rs
```

to undo the plant. `dialing.rs` held both the mutation **and** the
uncommitted fix, so that reverted both. I then committed without reading
the staged diff and wrote a message saying the correction had landed and
been verified.

**The result is worse than a plain miss.** The commit message and the PR
comment both asserted a change the code did not contain, so `git log` gave
the next reader no reason to re-check. A review caught it by running
`git log -S` on the corrected expression and finding it nowhere.

**How to apply.**

- Commit the fix, THEN plant mutations. A committed fix cannot be eaten by
  a restore, and `git checkout --` becomes the safe way to undo the plant.
- If the fix must stay uncommitted, copy the file to the scratchpad first
  and restore from that copy, never from `HEAD`.
- Plant mutations in a DIFFERENT file from the fix where possible.
- Read the staged diff before committing and confirm the hunk you care
  about is actually in it — `git diff --cached -- <file> | grep` for the
  new expression. CLAUDE.md §9 already requires inspecting the whole
  staged diff; this is what it is for.

**IT HAPPENED A SECOND TIME, 2026-09-10, on PR #85 — and this one hid.**
Four uncommitted comment corrections in
`tools/checks/check_vendored_advisories.sh`, then a mutation plant in the
SAME file, then `git checkout -- "$G"` to undo the plant. All four edits
went with it.

**What made it worse than the first instance: the self-test still passed.**
The new assertions I had just written satisfied the restored HEAD guard for
unrelated reasons — the exit-4 clause already existed (it simply had no
fixture), the BOM was already tolerated — so a full green run came back
after the loss and read as confirmation. I only noticed because a later
edit could not find its own anchor text.

So "read the staged diff" is not sufficient on its own here: a green
verification run AFTER a restore proves nothing about whether your edits
survived it. Grep for a distinctive phrase from each edit before staging.

And note the shape: the plant was in the same file as the fix *because the
fix was a comment in the file the guard reads*. When a guard's subject is
its own source, "plant in a different file" is not available — so commit
first is the only protection left.

Related: [[mutation-checks-need-the-formatted-text]],
[[verification-pipelines-must-fail-loudly]], [[assertions-that-cannot-fail]].

*References: assertions-that-cannot-fail, mutation-checks-need-the-formatted-text, verification-pipelines-must-fail-loudly*
