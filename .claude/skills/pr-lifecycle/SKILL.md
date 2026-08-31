---
name: pr-lifecycle
description: Taking a change from a fresh branch to a merged PR in the InterWeave repository — the working, integration, opening, landing and follow-up phases, plus concurrent-session and subagent-dispatch rules. Use whenever you are about to commit, push, open a PR, arm a merge, or answer review findings. For where a branch comes from and when a security-boundary change must wait for review, CLAUDE.md §9 keeps those; they are needed before you know this skill applies.
---

# Landing a change

`CLAUDE.md` §9 carries what a session needs *before* it knows this skill
applies: the branch-per-task rule and its five orientation steps, the fact
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
17. gh pr create --base main --head "$BRANCH"
18. gh pr merge <n> --auto                     # ONLY when done — nothing asks
19. tools/gh/wait-merged.sh <n> &              # background; its exit is the callback
19b. tools/gh/pr-review-status.sh <n> --wait 30m --automated-only &
```

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
| `pr-review-status.sh <n>` | was this PR *actually* reviewed, by whom, and against which head |
| `pr-sessions.sh` | which session owns which PR; `/unresolved` lists PRs still owing a reply |
| `pr-reply.sh` | reply to and resolve one review thread, body on STDIN |
| `actions-health.sh` | is Actions healthy enough to be worth spending a run on |

Two things about those exit codes that are easy to misread, and are not
obvious from a single run:

- `BLOCKED` from `wait-merged.sh` while checks are merely pending is the
  normal waiting state, not a verdict.
- `pr-review-status.sh` exit 5 returns immediately rather than sitting out
  the timeout, so a fast `--wait` there is an answer and not a failure.

## When to open a NEW PR

**A PR waiting on review does not block the next task.** Review rounds take
minutes to hours and a session that idles through them wastes most of its
time, so start the next piece of work on its own branch rather than
waiting. Come back to the open PR when its review lands.

What makes that safe is that each task is a separate branch off fresh
`origin/main`, so concurrent work shares nothing but the base. The two
rules that keep it that way — partition by FILE SET rather than intent, and
a task depending on another waits for it to be MERGED — are start-of-task
decisions, so they live in `CLAUDE.md` §9 where they are loaded before you
choose the work.

Within one branch, the old rule stands: if the work belongs to the task the
branch is for, it is another commit on it, not a second PR. "Different
concerns", "different packages" and "different root causes" are commit
boundaries, satisfied by committing separately. A batch past ~6–8 commits
is a reason to stop adding and land, not to open a second PR alongside.

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
  The `<host>/<clone>/…` prefix tells you whose it is. Same for their PRs:
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

**ONE EXCEPTION: a review the automated reviewer declined.** When
`@codex review` answers "You have reached your Codex usage limits", the
gate in `CLAUDE.md` §9 reads as satisfied while nothing has been
reviewed — `pr-review-status.sh` counts a refusal as "already answered".
Dispatch a reviewer without being asked, one per PR, and read §9's
"When the reviewer declines" for the rest: no session context passed,
and the findings posted to the PR and answered there rather than
reported into the transcript — including when there are none, since a
clean review that leaves no comment is indistinguishable from the
refusal that preceded it. The model is `opus` by the standing rule
below, not by anything special about this path.

The `PreToolUse` hook in `.claude/settings.json` denies a dispatch missing
`model` or `isolation` and states why at the moment of the call, so those
two requirements are not restated here. **A dispatch whose `description`
names it a review is exempt from both the isolation requirement and the
premium-model prompt** — a review writes nothing, so it reads the session
tree with no worktree, and `opus` is standing for it (`CLAUDE.md` §9).
That exemption is narrow on purpose, and a review must be NAMED to get
it: the description has to BEGIN with `review` or `re-review`, and the
model has to be `opus`. Both halves earn their place — matching
`review` anywhere let `Address review feedback` through, which is a
WRITING dispatch that would then have run with no worktree in the
session clone; and without the model condition a `sonnet` or `fable`
dispatch could take the exemption and evade the very rules it sits
beside.

**Which model, though, is a decision the hook cannot make for you.** It
denies a *missing* `model`; on `opus` or `fable` it only **asks**, and an
ask is answered by the user, not by the rule. So:

- **`opus` and `fable` are FORBIDDEN as subagent models** unless the user's
  prompt explicitly asks for that tier for that dispatch. "The task looks
  hard" is not authorisation; neither is "the session is already running
  that model" — the hook's own text says the inheritance is the failure
  mode, not the default. **CODE REVIEW IS THE STANDING EXCEPTION** and
  needs no per-dispatch authorisation: every review subagent runs on
  `opus`, whether the automated reviewer declined, a review was asked
  for directly, or merged code is being audited. `CLAUDE.md` §9 carries
  the rule and why — a review's failure mode is not a retry, it is a
  green PR that merges.
- **Choose the cheapest tier that can do the job**: `haiku` for mechanical,
  well-specified work (extraction, pattern-following edits, structured
  search); `sonnet` for judgement work (multi-file reasoning,
  convention-holding prose). NOT reviews — those are `opus` by the
  standing rule above, and this sentence listing them as `sonnet` work
  is what invited a hook exemption wide enough to let a `sonnet` review
  through it.
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
