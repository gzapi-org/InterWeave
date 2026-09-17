---
role: "p2p-network-dev"
class: workflow
description: "A known-failing local check hides new failures behind it; run the CI-equivalent command and suppress the known lint rather than reading past it"
tier: 1
knowledge_scope: full
distilled_at: "2026-09-17"
origin:
  - agent: user
    host: "develop-qzapp"
    project: interweave
    working_copy: InterWeave
derived_from:
  - d7e1c42f5d961899
---

## A known-failing local check hides new failures behind it; run the CI-equivalent command and suppress the known lint rather than reading past it

**A check you have labelled "known-failing" stops being read, and then it
hides the next real failure.**

On InterWeave this VM has no rustup, so the system 1.98 toolchain raises
`clippy::chunks_exact_to_as_chunks` on `crates/api/transport-api/src/ids.rs`
that the pinned 1.97.1 does not. That one lint was treated as noise for
several rounds — and it masked a genuine new error two ways at once:

- I ran `cargo clippy` **without** `-- -D warnings`, which exits 0 where
  CI's invocation exits 101. A new `unused_assignments` was a warning
  locally and an error in CI.
- The known error **aborts compilation of that crate**, so crates
  downstream of it may not be linted at all.

CI caught it; I had reported the branch green in the same breath.

**How to apply:** run what CI runs —
`cargo clippy --workspace --all-targets -- -D warnings` — and get to
exit 0 by *temporarily* adding `#[allow(...)]` at the known site, then
reverting. Never by filtering the output or by grepping for the one
message you expect. If a check is known-failing, the local workaround
must make it PASS, or it is not being run.

**And `-p <package>` is not the CI invocation either.** On 2026-09-10 a
commit that MOVED a function out of `direct_codec.rs` left its eleven-line
doc comment attached to the next function down. `empty_line_after_doc_comments`
catches that, CI runs `-D warnings`, and I missed it because I had linted
only the package I thought I had edited. The scope is `--workspace`, always:
a deletion's damage lands in the file you stopped looking at.

**And a second WORKTREE needs its own run.** On 2026-09-10 I had two
branches checked out at once — the session clone and a `git worktree` for a
parallel PR — and ran `cargo fmt --all` in the session clone after editing a
file in the worktree. CI's formatting step failed. "I ran it" is only ever
true of the directory it ran in, and `--all` means all workspace members, not
all checkouts.

Related: [[verification-pipelines-must-fail-loudly]] — same root shape,
a verification whose exit code is not actually being consulted.

**USE `--no-fail-fast`, because cargo stops at the first failing target.**
On 2026-09-10 a load-sensitive real-socket test in `tests/pubsub` timed out
while a review agent and a build competed for this VM's two cores. `cargo
test --workspace` then reported 45 passing targets out of 65 — not because
45 was the real number, but because it halted. Read alone and unloaded the
same test passes in 0.26s against a 20s timeout, and CI had already passed
the same commit.

Two consequences: a flake hides every target after it, and a truncated count
reads like a smaller suite rather than an aborted run. Always
`--no-fail-fast` for a verification run, compare the target count against
what you expect, and before blaming a branch for a timing failure check
whether CI passed the same head and whether the machine was busy.

*References: verification-pipelines-must-fail-loudly*
