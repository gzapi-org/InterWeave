---
name: pr-lifecycle
description: Taking a change from a fresh branch to a merged PR in the InterWeave repository — the working, integration, opening, landing and follow-up phases, plus concurrent-session and subagent-dispatch rules. Use whenever you are about to commit, push, open a PR, arm a merge, or answer review findings. For where a branch comes from and when a security-boundary change must wait for review, CLAUDE.md §9 keeps those; they are needed before you know this skill applies.
---

# Landing a change

`CLAUDE.md` §9 carries what a session needs *before* it knows this skill
applies: the branch-per-batch rule and its five orientation steps, the fact
that nothing prompts before code lands, and the requirement that a
security-boundary change wait for review. This skill is the rest.

## The phases

Phase 1 — starting a task — is in `CLAUDE.md` §9, because you need it
before you are anywhere near a PR.

**Phase 2 — working**

```
6.  implement ONE fix / ONE package
7.  add its tests
8.  run the impacted tests
9.  commit (one per root cause, per package)
10. git push
```

Commits are cheap and local, and pushing is ungated — so push whenever the
branch is worth showing, and always before asking for a review, because a
review reads what is on the remote and nothing else.

**Phase 3 — integration**

```
11. git fetch
12. git merge --no-ff origin/main
13. build + test the MERGED tree               ← the step people skip
14. bash tools/checks/scan_semantic_collisions.sh
15. bash tools/checks/check_license_headers.sh
16. git push
```

Step 13 matters because required checks are **not strict**: a branch can
be green against a base it was never built on. Step 14 catches what a
textually-clean merge hides — two sessions minting the same ADR number in
different files, or the same amendment heading. Step 15 catches licence
terms that rode in with copied material.

**Phase 4 — opening**

```
17.  gh pr create --base main --head "$BRANCH"
17a. Agent(code-review, fable, "Review PR <n> …", no isolation)  # THE review of the finished head
17b. tools/gh/post-review.sh <n> <<'EOF' … EOF   # post it at the head; pr-review-status.sh counts it
17c. tools/gh/pr-review-status.sh <n>            # blind reviews against the CURRENT head; unresolved threads
18.  gh pr merge <n> --auto     # ONLY when done, AFTER 17a is posted with no open P1/P2 —
                                #   and, on a security boundary, on the owner's word. Nothing asks.
19.  tools/gh/wait-merged.sh <n> &      # background; its exit is the callback
```

**17a is the review, and 17b is what makes it count** — a review that is
not posted at the head is coverage of nothing, and `pr-review-status.sh`
reads the posted object, not the transcript; CLAUDE.md §9 says why the
session dispatches it (the automated reviewer this repository once asked
for by comment is retired; the section there keeps the record). **18 sits
below them deliberately**: arming is standing consent, so it belongs after
the gate rather than above it.

Drop `--delete-branch`: the queue owns branch cleanup and `gh` rejects the
flag.

**Phase 5 — landing**: the queue builds the head on current `main`, merges,
and pushes. Return to `main` deliberately, on the notification — never let
a background watcher move your tree.

**Phase 6 — after**

```
20. tools/gh/pr-sessions.sh /unresolved        # what still owes a reply
21. tools/gh/pr-review-status.sh <n>           # was it REALLY reviewed, and against which head
22. tools/gh/pr-reply.sh … <<'EOF'             # reply + resolve one thread; body on STDIN
```

Phase 6 is not optional and nothing reminds you: no red check, no blocked
merge. A PR can and does merge with findings outstanding.

**Answering findings is a TASK, so it starts at Phase 1 like any other** —
fetch, check where you are, cut a fresh branch. The findings arrived on a
branch that is already merged and deleted, so whatever you are standing on
is by definition the wrong place. Any code fix lands on a new branch; only
the replies go to the old PR.

Never post a reply body through `gh api -f body="…"`: replies quote code,
and a double-quoted body has its backticks command-substituted and its
`$vars` expanded before `gh` sees them. `pr-reply.sh` takes the body on
STDIN for exactly this reason — the same hazard as the heredoc rule for
commit messages.

## The tooling

`tools/gh/` scripts are the supported way to observe a PR. **Each carries
its own `--help`, and that is where the flags, semantics and exit codes
live** — read it rather than trusting a table written elsewhere, because a
table in prose goes stale and `--help` ships with the script. Each has a
self-test beside it (`test_*.sh`) that must stay green.

| script | what it answers |
|---|---|
| `wait-merged.sh <n>` | blocks until the PR reaches a terminal state; run it in the **background**, its exit IS the callback |
| `pr-review-status.sh <n>` | was this PR *actually* reviewed, by whom, and against which head — a head is reviewed when a blind review (the class's, posted with `post-review.sh`) or an independent review targets it |
| `post-review.sh <n>` | post the review class's review as a review object at the head, body on STDIN, marked so the reader counts it |
| `pr-sessions.sh` | which session owns which PR; `/unresolved` lists PRs still owing a reply |
| `pr-reply.sh` | reply to and resolve one review thread, body on STDIN |
| `actions-health.sh` | is Actions healthy enough to be worth spending a run on |

Two things about those exit codes that are easy to misread, and are not
obvious from a single run:

- `BLOCKED` from `wait-merged.sh` while checks are merely pending is the
  normal waiting state, not a verdict.
- `pr-review-status.sh` reports a review of an earlier head as not on head:
  a fix range is re-reviewed before the arm, however small.

## When to open a NEW PR

**A PR waiting on review does not block the next task.** Review rounds take
minutes to hours and a session that idles through them wastes most of its
time, so start the next piece of work on its own branch rather than
waiting. Come back to the open PR when its review lands.

What makes that safe is that each batch is a separate branch off fresh
`origin/main`, so concurrent batches share nothing but the base. The two
rules that keep it that way — partition by FILE SET rather than intent, and
a task depending on another waits for it to be MERGED — are start-of-task
decisions, so they live in `CLAUDE.md` §9 where they are loaded before you
choose the work.

Within one branch, the old rule stands: if the work belongs to the batch
the branch is for, it is another commit on it, not a second PR. "Different
concerns", "different packages" and "different root causes" are commit
boundaries, satisfied by committing separately. So is "a different small
task": a PR is a review unit, and `CLAUDE.md` §9 sets the floor at eight
work commits before arming without a fresh ask — two small pieces of work
ready at the same time are one PR, not two (the owner, 2026-09-19). A
batch past about sixteen is landed and the rest starts a new batch, not
a second PR alongside; a second PR is for work that cannot share the
review — a landing another task depends on, a file another branch
refactors, another session's lane.

**Track what is outstanding.** With several PRs open,
`tools/gh/pr-sessions.sh /unresolved` is the list of what still owes a
reply, and Phase 6 applies to every one of them.

## Dequeuing a PR that is already queued

If a finding lands on a PR that is already queued:

```
gh api graphql -f query='mutation{dequeuePullRequest(input:{id:"<pr node id>"}){mergeQueueEntry{state}}}'
```

`gh pr merge --disable-auto` clears the standing consent but does **not**
dequeue.

## Concurrent sessions and worktrees

Multiple sessions may work this repository in parallel, possibly on
different hosts. **Isolation is required**, and the model is **one full
`git clone` per session**. Sessions coordinate **only** through `origin`.

- **Your starting directory is yours exclusively**, and the rule for what
  to do when it is not — stop and report, never reset — lives in
  `CLAUDE.md` §9 beside the `reset --hard` it guards, because that step
  runs at task start and this skill loads later.
- **Never push to, rebase, or delete a branch another session created.**
  The `<host>/<login>/…` prefix tells you whose it is (older branches carry the clone directory there; the forwarded tools read both). Same for their PRs:
  do not retarget, re-title, or merge them. Only answer review comments on
  PRs you opened — `pr-reply.sh` refuses another session's PR, but the rule
  is yours to keep, not the script's.
- **Never resolve a review thread you have not actually addressed.**
  Resolving signals "this was handled"; a false resolve buries the finding
  with nothing left to catch it.

## Subagent dispatch

**Dispatching agents is OPT-IN: fan out only when the user asks for it.** A
task that merely looks parallelisable is not an invitation. The isolation
contract governs **how** to dispatch, never **whether**.

**ONE EXCEPTION: code review.** Every finished head dispatches the review
class WITHOUT being asked, one per PR — read §9's "The review is the
review class's, dispatched by the session" for why there is no other
reviewer to wait for and for the rest of the contract: no session context passed, the brief scoped to the PR's
`base..head` range, and the findings posted to the PR and answered
there rather than reported into the transcript — including when there
are none, since a clean review that leaves no comment is
indistinguishable from a review that never happened. The model is
`fable`, the alias the review class rides, by the standing rule below —
not by anything special about this path.

The `PreToolUse` hook in `.claude/settings.json` is agent-fabric's
dispatch guard (`runtime/claude-code/hooks/agent-dispatch-guard.sh`,
beside this checkout — the same command gzapp wires). It keys on the
type: a dispatch with no `model` is denied for every type but `fork`
(which continues the session); a coding
class (`code-low`/`-medium`/`-high`/`-plan`) is denied on any alias but
its own and without `isolation: "worktree"`; the read-only harness types
(`Explore`, `Plan`, `claude-code-guide`) are denied WITH isolation — they
cannot write, and a worktree would hide the uncommitted work they are
asked about; and it ASKS, rather than decides, on the premium coding
classes (`code-high`, `code-plan`) and on a premium tier for a type the
class table does not pin. It states why at the moment of the call, so
those rules are not restated here. **The review class is exempt from
both the isolation requirement and the premium-model prompt** — a
review writes nothing, so it reads the session tree with no worktree,
and `fable` is standing for it (`CLAUDE.md` §9). That exemption is
narrow on purpose, and a review must be NAMED to get it, all four at
once: `subagent_type: "code-review"`, a description that BEGINS with
`review` or `re-review`, `model: "fable"`, and no `isolation`. Each
condition earns its place — matching `review` anywhere let `Address
review feedback` through, which is a WRITING dispatch that would then
have run with no worktree in the session clone; and without the class
and model conditions a `sonnet` dispatch, or a coding class on `fable`,
could take the exemption and evade the very rules it sits beside.

**Which model, though, is a decision the hook cannot make for you.** It
denies a *missing* `model`, and a coding class on an alias other than its
own; on the premium coding classes, and on `opus` or `fable` for a type
the class table does not pin — a read-only type, or an unclassed one —
it only **asks**, and an ask is
answered by the user, not by the rule. So:

- **`opus` and `fable` are FORBIDDEN as subagent models** unless the user's
  prompt explicitly asks for that tier for that dispatch. "The task looks
  hard" is not authorisation; neither is "the session is already running
  that model" — the hook's own text says the inheritance is the failure
  mode, not the default. **CODE REVIEW IS THE STANDING EXCEPTION** and
  needs no per-dispatch authorisation: every review is a `code-review`
  dispatch on `fable`, the alias the class rides — the one on every
  finished head, one that judges a finding, a re-review, or an audit of
  merged code. `CLAUDE.md` §9 carries
  the rule and why — a review's failure mode is not a retry, it is a
  green PR that merges.
- **Choose the cheapest tier that can do the job**: `haiku` for mechanical,
  well-specified work (extraction, pattern-following edits, structured
  search); `sonnet` for judgement work (multi-file reasoning,
  convention-holding prose). NOT reviews — those are `code-review` on
  `fable` by the standing rule above, and this sentence listing them as
  `sonnet` work is what invited a hook exemption wide enough to let a
  `sonnet` review through it.
- **Fan-out multiplies cost by the agent count**, so a large wave is a
  reason to drop a tier, not to keep the session's.

**The hook only speaks when it DENIES.** A compliant dispatch sees nothing
from it, so everything the agent itself must be told has to come from the
prompt you write:

- **The agent stays inside the worktree path it is given, runs no git at
  all, and never commits.** It does not create, enter, exit, or remove a
  worktree. An agent that commits or moves its worktree can make its own
  output uncollectable, or destroy the only copy of it when the session
  performs the force-removal below.

What the hook does not cover either:

- **Both apply inside a `Workflow` script too, and NOTHING ENFORCES THEM
  THERE.** The hook matches the tool name `Agent`, so a workflow's
  `agent(prompt, opts)` calls never reach it, and both fields default the
  wrong way. Write both out on every call:
  `agent(prompt, { model: 'haiku', isolation: 'worktree' })`.
  **The premium-tier rule above is unenforced there in a way it is not for
  a direct `Agent` call**: no ask arrives, so `model: 'opus'` across a wave
  of agents costs what it costs in silence. The rule is the only thing
  standing between a workflow and that bill.
- **`worktree.baseRef` is pinned to `"head"`** in `.claude/settings.json`.
  The default (`fresh`) branches agent worktrees from
  `origin/<default-branch>`, so the agent sees **none** of the unmerged
  task branch — exactly when fan-out is worth doing. Do not revert it, and
  check it before diagnosing a "blind" agent.
- **`head` is the committed HEAD, not your working tree — so COMMIT BEFORE
  YOU FAN OUT.** Uncommitted edits and untracked files are invisible to
  every agent, and an agent asked to extend work you have not committed
  silently reads the *previous* version and reports success against it.
- **Only the session commits.** Collecting an agent's work is a **copy**,
  not a merge: read what it produced, apply it in the clone yourself, and
  commit there. The `worktree-agent-<id>` branch is a by-product of
  isolation, not a delivery mechanism.
- **Partition WRITES by file set, and name that set in each prompt.**
  Isolation made concurrent mutation safe; it did not make it collectable.
  Two agents that both rewrite the same file hand the session two divergent
  versions and no merge. Overlapping reads are free.
- **The harness opens the worktree; the session closes it whenever the
  agent did work.** The harness auto-removes only worktrees left unchanged,
  so every productive dispatch leaves one behind:
  ```
  git worktree list
  git worktree remove --force <path> && git branch -D worktree-agent-<id>
  git worktree prune
  ```
  `.claude/worktrees/` is gitignored because it sits inside the repository
  and each entry holds a `.git` file — without the ignore, `git add -A`
  stages it as an embedded repository and commits a broken gitlink.
