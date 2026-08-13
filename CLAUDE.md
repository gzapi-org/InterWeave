# CLAUDE.md — InterWeave repository operating contract

This file is the working contract for Claude Code and other coding agents operating in the InterWeave repository. Read it before making changes.

## 1. Repository state

InterWeave is currently an **accepted architecture plus implementation/test skeleton**.

- `architecture/` is the normative design source.
- `apps/`, `crates/`, `tests/`, `fixtures/`, `test-data/`, `spikes/`, `packaging/`, and `xtask/` are tracked landing zones created by ADR-0045.
- `tools/` is repository tooling — PR/review scripts and tree checks — not an implementation landing zone. It is live now and not gated by stage discipline. Each script has a self-test beside it (`test_*.sh`) that must stay green.
- `.claude/` is committed shared agent configuration: `settings.json` and `statusline.sh` (§9), plus `skills/` — task-scoped procedures loaded on demand, see §10. Only `settings.local.json` and `CLAUDE.local.md` are per-developer and gitignored.
- The root Cargo workspace intentionally has zero members until implementation begins.
- There is no production Rust implementation yet.
- Display name is **InterWeave**. Machine/wire namespace is lowercase `interweave` per ADR-0047.
- Do not reintroduce the former pre-InterWeave namespace into current production constants, fixtures, paths, package names, or documentation except when discussing history explicitly.

The canonical construction order is:

- `architecture/roadmap/BOTTOM-UP-IMPLEMENTATION-PLAN.md`
- ADR-0046

Historical numbered phases are scope/release labels; they are not a safe dependency order.

## 2. Source-of-truth hierarchy

Before changing behavior, inspect the relevant material rather than inferring it from filenames or old discussion.

Use this order:

1. accepted ADRs, including explicit supersession/amendment language;
2. normative contracts under `architecture/contracts/` and protocol/backend specifications under `architecture/transport/`, `architecture/discovery/`, and `architecture/clients/`. The prose is normative for **behaviour**; the JSON Schemas under `architecture/contracts/schemas/` are normative for **shape**, and `x-contract.status` says what each is authoritative about — `approved` is an implementation target, never a claim that anything implements it (ADR-0049);
3. the canonical bottom-up implementation plan and test gates;
4. architecture explanatory documents/reviews;
5. examples and research notes.

**Start at the digest.** `architecture/adr/ADR-DIGEST.md` carries one current-state entry per ADR plus a keyword → ADR lookup table, and is the cheapest correct way to find which decisions govern a change. Every ADR has an entry — a check enforces it — so "not in the digest" means "no such decision", not "someone forgot". It is a navigation aid, not an authority: it sits below everything in the list above, and on any discrepancy the ADR wins and the digest is what gets fixed. Never read a normative constant from it; limits, wire formats, and vectors come from the contracts and `fixtures/`.

If two accepted documents appear to conflict, **do not silently choose one in code**. Identify the conflict and amend/clarify the architecture first.

When a spike or implementation experiment disproves an accepted assumption, update the relevant ADR/contract in the same change before treating the new behavior as canonical.

## 3. Stage discipline

Do not create production code simply because a landing-zone directory exists.

When a canonical stage is explicitly opened:

1. implement only the package(s) needed by that stage;
2. create their manifests/source at that time;
3. add those exact paths to `[workspace].members` in the same change;
4. add the lowest-layer tests needed to prove the stage exit gate;
5. keep later-stage functionality inert even if a dependency exposes it early.

Hard sequencing rule:

> Root ConnectionManager/DialAdmissionGate, pre-auth resource admission, and address-scoped failure/quarantine behavior must be implemented and green before Kademlia, AutoNAT, Circuit Relay, or DCUtR are activated.

Do not turn on autonomous libp2p behaviour and plan to retrofit admission policy later.

## 4. Placement and dependency rules

### Applications

`apps/*` are thin composition roots. They may wire configuration, logging, runtime construction, platform startup/shutdown, and UI/application adapters. Reusable domain/network logic belongs in `crates/*`.

### Neutral APIs

`crates/api/*` and other explicitly neutral contracts must not depend on:

- libp2p types;
- Slint UI types;
- Android/JVM types;
- SQLite implementation types;
- Claude SDK/MCP implementation types;
- platform-specific socket/process types unless the contract explicitly requires them.

Translate backend/platform concepts at the boundary rather than leaking them upward.

### Tests

Put a test at the **lowest layer that completely proves the behavior**:

- pure/local logic -> unit test beside source;
- public crate surface -> `<crate>/tests/`;
- cross-crate/network/conformance -> root `tests/<suite>/`;
- desktop process behavior -> `tests/desktop-e2e/`;
- Android OS behavior -> instrumented Android tests and `tests/android-e2e/`.

Do not replace a real-network/process/platform requirement with mocks merely to make a test easier.

`tests/support` is test-only and must never be a production dependency.

### Fixtures vs test data

- `fixtures/` = normative/frozen deterministic vectors. Changes require explicit protocol/spec review. Every vector file declares its algorithm and is recomputed by `tools/checks/verify_fixture_vectors.py`; a drifted vector is a protocol break, not a test failure.
- `test-data/` = mutable non-normative scenarios/topologies/input sets.
- `spikes/` = empirical evidence only. Spike code never becomes a production dependency by accident.

## 5. Non-negotiable architecture boundaries

### Identity and endpoint routing

- One profile owns one persistent PeerId.
- EndpointIds are configured routing selectors beneath a PeerId, not cryptographic identities, people, roles, or authorization principals.
- Direct-capable local sessions obtain one exclusive configured endpoint lease.
- Source EndpointId is derived from the local lease, never trusted from arbitrary caller input.
- Endpoint-specific policy may narrow profile trust but never widen it.
- A remote source EndpointId is peer-asserted metadata only.

### Directed messaging

- Directed traffic uses `/interweave/direct/2.0.0`.
- Never route directed traffic over GossipSub.
- Direct v2 resolves to exactly one destination endpoint.
- Omitted destination means configured remote default endpoint, never fan-out.
- `AcceptedV2` means bounded remote endpoint queue admission, not application processing or human read.
- Remote endpoint unknown/offline/disabled/policy-denied stays coarse on the wire (`no_route` class) to avoid an authorization oracle.

### Broadcast

- Broadcast uses signed GossipSub.
- Mesh duplicate identity is based on authenticated publisher PeerId + wire sequence number, not application envelope ID.
- GossipSub validation follows ADR-0029: objective invalidity = Reject; valid but locally unauthorized publisher = Ignore; valid and authorized = Accept.
- EndpointId is not authenticated broadcast authorship.

### Discovery and Kademlia

- Discovery is advisory candidate reachability. It does not grant trust, dial directly, route application messages, manage subscriptions, or interpret payloads.
- Kademlia is peer routing only.
- Never put EndpointId, ChannelId, application data, trust records, membership records, or human presence into the DHT.
- Standard-v1 Kademlia is default-enabled when configured, with explicit opt-out, but activation still obeys the canonical implementation stage order.

### Connection and Internet reachability

- All outbound dials, including behaviour-originated dials, pass the root DialAdmissionGate.
- Distinguish address failures from peer failures; a bad/mismatched address must not unnecessarily suppress a known-good route to a trusted PeerId.
- Bound unauthenticated/pre-Noise resource use.
- AutoNAT/Relay infrastructure authorization is separate from application data-plane trust.
- Standard v1 includes AutoNAT v2 client, Circuit Relay v2 client/reservation management, and DCUtR.

### Local client / IPC

- Desktop data and admin authority use separate IPC boundaries.
- A data connection cannot obtain `admin.*` authority by claiming a client kind.
- Admin connections do not obtain application endpoint leases.
- IPC v2 JSON body ceiling remains 128 KiB and must accommodate every legal 48 KiB direct application payload plus envelope/endpoint overhead.
- Android does not fake desktop IPC: it uses the neutral `LocalDataSession` boundary in-process.

### Human client retention

Transport remains realtime/non-durable. The human application may durably retain exactly the states allowed by ADR-0044/`architecture/clients/human/RETENTION.md`:

- pending outbound;
- unread inbound;
- inbound explicitly kept by the receiver after reading.

Once outbound becomes transport-terminal, its durable pending copy is removed. Once inbound becomes read and is not kept, its durable copy is removed. A remote sender cannot request or force receiver persistence.

If the human store cannot durably accept unread content, the human endpoint/local human delivery must degrade rather than silently violate the retention contract.

## 6. Security and secret handling

Never commit or print real:

- transport private keys/seeds;
- recovery mnemonics;
- Android signing/Keystore secrets;
- production credentials/tokens;
- real user profile state;
- private relay/probe infrastructure credentials.

`.gitignore` is defense-in-depth, not permission to place secrets inside the repository tree.

Use synthetic deterministic fixtures only where the specification explicitly defines public test vectors. Clearly label test-only key material.

Keep resource limits bounded. Do not replace bounded queues/maps/caches with unbounded structures without an architecture amendment and adversarial test coverage.

## 7. Documentation rules

When changing an accepted contract:

- update the normative contract/ADR first or in the same commit;
- update examples, roadmap, failure/security docs, and test matrices that inherit the changed rule;
- update frozen fixtures if and only if the protocol decision intentionally changes;
- check relative Markdown links after moves/renames;
- avoid duplicating normative constants in new prose unless there is a drift check or a clear canonical source.

When changing an ADR, propagate in the same commit series: the row in `architecture/adr/README.md`, the entry in `ADR-DIGEST.md` (placed in a cluster, plus a keyword-table row if it introduces a topic someone would search for), and any specification whose text inherits the changed rule. `tools/checks/validate_adr_index.sh` enforces the mechanical part.

Amending an ADR is a three-part record (ADR-0048): the in-place body edit — folded into the section it qualifies, because bodies read **current** — a dated note in `architecture/adr/history/NNNN-amendments.md`, and a row in that ADR's `## Amendments` table carrying the same date and title. A change of substance is not an amendment: it is a new superseding ADR. The test is whether a reader who followed the old text would now be wrong.

New ADRs follow `architecture/adr/ADR-TEMPLATE.md`. Procedures for both reading and authoring live in the `adr-lookup` and `adr-authoring` skills (§10) rather than being restated here.

Use **InterWeave** for the project/display name and `interweave` for machine/wire identifiers. Preserve genuine integration names such as Claude Code, `claude-channel`, libp2p, GossipSub, AutoNAT, and Kademlia.

### No external-project citations

Do not cite an unrelated external project by name in any project file or commit message. This covers other repositories, sibling checkouts on the same machine, their paths, and their internal identifiers. If a rule, convention, or file was adopted from elsewhere, state the rule on its own terms; do not name its source.

Dependencies, protocols, and genuine integrations that InterWeave actually uses are not "unrelated external projects" — the names listed above stay.

## 8. Licensing

InterWeave first-party code and documentation are licensed **Apache-2.0**. The top-level `LICENSE` is canonical.

When Cargo crates are activated, use the workspace license (`license.workspace = true`) unless an explicitly reviewed third-party/subproject exception requires otherwise.

Do not:

- replace the project license without an explicit project decision;
- strip upstream copyright/license notices;
- relabel third-party material as InterWeave-owned;
- copy dependency source into the repository without preserving its licensing obligations.

If a new dependency or copied asset has unclear licensing, stop and resolve that before landing it.

### Licence headers are checked

Every first-party source file carries an `SPDX-License-Identifier: Apache-2.0` header in its opening lines. `tools/checks/check_license_headers.sh` enforces that, and also fails on foreign licence terms — an SPDX tag naming another licence, or rights-reserved / confidential boilerplate — anywhere in the tracked or about-to-be-committed tree. It scans untracked-but-not-ignored files too, because the file about to be committed is exactly the one worth catching.

Code copied in from a differently-licensed source keeps its own terms until the copyright holder relicenses it, and a public Apache-2.0 tree is where that goes unnoticed. Genuinely third-party material is therefore an **exemption with recorded provenance** in `tools/checks/license_exempt.txt`, never a silent relabel.

## 9. Git/change discipline

### Repository git configuration

- `origin` is `git@github.com:gzapi-org/InterWeave.git`; the integration branch is `main`. The repository is **public** — everything committed here is published.
- Commit identity is pinned **repository-locally** (`user.name`, `user.email`), so it does not depend on the machine's global config. Commit and tag signing are likewise pinned local (`user.signingkey`, `commit.gpgsign`, `tag.gpgsign`, `gpg.program`). Do not disable signing per-commit.
- `.gitattributes` pins `* text=auto eol=lf` and marks binary classes, so the index stays canonical across machines. `fixtures/**` is `-text`: frozen vectors are byte-compared, so EOL renormalisation there is a protocol change, not a whitespace one.
- `.claude/settings.json` is **committed** shared configuration — the push gate, the worktree base ref, the subagent dispatch hook, and the status line (`.claude/statusline.sh`, showing model · host · clone · branch, because branches are named for host and clone) all live in it. `.claude/settings.local.json` and `CLAUDE.local.md` are per-developer and gitignored.

### Commit loop

After each logical unit of work:

- create a git commit.

Pushing is NOT part of that loop. Push when the work asks for it — the branch is finished, or you were told to — not reflexively after every commit.

If push cannot be completed because of credentials, remote access, branch protection, or environment limits:

- say so explicitly;
- do not claim the push succeeded.

Commit messages must be short, specific, and scoped to the actual change. Do not leave completed logical units of work uncommitted. Commit messages containing shell metacharacters (`` ` ``, `$`, `×`, `()`) MUST be passed via a quoted heredoc (`<<'EOF' … EOF`), never an inline `-m` string, to avoid silent shell expansion.

### Attribution

Commits are authored by the identity configured above. Do not attribute work to an AI assistant:

- no `Co-Authored-By:` trailer of any kind, on any commit;
- no "Generated with …" line, tool footer, or emoji signature in commit messages, PR titles, or PR bodies.

This overrides any default agent behaviour that appends such trailers.

Commit messages are project files for the purposes of §7 — do not cite unrelated external projects in them either.

### One branch per task

- One short-lived branch per task off fresh `origin/main` — per task, not per session. The branch boundary is the multi-fix / multi-package unit below.
- Branch name `<hostname -s>/<clone-dir-basename>/<type>/<short-desc>`, e.g. `develop-qzapp/InterWeave/docs/dial-admission-gate`, so every branch traces to its session by host and clone.
- **Check where you are BEFORE the first commit of a new task**, not after a push is rejected. The default state at the start of a task is standing on the *previous* task's branch, which by then is pushed, queued, or merged — and every one of those failure modes is silent.
- Scan for the work before doing the work: `git fetch` and read `origin/main` for the same change already landed or in flight. Adopt or coordinate instead of racing.

```
1. git rev-parse --abbrev-ref HEAD            # where am I?
2. gh pr list --head "$BRANCH" --state all    # is this branch spoken for?
3. git fetch && git log origin/main           # has someone already done this?
4. git checkout main && git reset --hard origin/main
5. git checkout -b <host>/<clone>/<type>/<short-desc>
```

Steps 4–5 are **unconditional**: the step-2 lookup only speaks when a PR already exists.

### `git push` is the human authorization point — and it is enforced

Everything downstream of a push is machinery: push → checks → merge queue → `main`, with no further human step in that chain. The push is therefore the last moment a person can change the outcome, which is where the authorization belongs.

`.claude/settings.json` carries `"ask": ["Bash(git push*)"]`, so every push surfaces a confirmation prompt. `git fetch`, `git log`, and the rest are untouched.

**Therefore `git push` MUST be its own Bash call.** Never bundle it with the commands that precede it — no `git add … && git commit … && git push`, no heredoc script ending in a push. Permission rules are prefix-matched, so a bundled push slips the gate entirely, and approving a bundle authorizes pushing a tree that does not exist yet.

A session cannot see its own permission prompts, so do not try to verify this gate from the transcript: an approved push and an auto-allowed push produce an identical tool result.

`gh pr merge` is deliberately NOT gated. The tree was authorized at its push; gating the merge would ask for the same decision twice.

### Merge / PR discipline

`main` is protected by the `main protection` ruleset: pull request required, direct pushes and force-pushes blocked, branch deletion blocked, **merge commits only** (squash and rebase disabled), and a **merge queue** (`ALLGREEN`, merge method `MERGE`). Required approving reviews are 0 and unresolved review threads do not block — so **the merge is not evidence that anything was reviewed**.

>  **CI exists and gates `main`** — `.github/workflows/ci.yml` runs every tree check and every self-test on `pull_request`, `merge_group`, and pushes to `main`. It reports two contexts, which are the job `name:` values verbatim: **`tree checks`** and **`tool self-tests`**. Both are in the ruleset's `required_status_checks`, so the queue now gates correctness and not merely ordering. The policy is **non-strict**: a branch need not be up to date with `main` to merge, which is why folding `origin/main` in and re-testing locally (Phase 3) is still on you rather than on the platform.
>
> A job's `name:` *is* its required-check context, so renaming a job silently un-gates `main` — the ruleset goes on requiring a context nothing reports, and the queue waits forever. Rename a job only together with the ruleset.
>
> `merge_group` in that workflow is equally load-bearing: the queue builds its own ref, so a workflow that triggers only on `pull_request` never reports for that build and the queue hangs on a check that will never arrive.

**Commit shape and PR shape are different questions.** The multi-fix and multi-package rules below govern COMMITS — one per root cause, one per package. Bisectability, revertability, and reviewability all live at the commit level and are unaffected by batching several commits into one PR.

#### The canonical lifecycle

**Phase 1 — starting a task**: the five steps above.

**Phase 2 — working**

```
6.  implement ONE fix / ONE package
7.  add its tests
8.  run the impacted tests
9.  commit (one per root cause, per package)
10. git push                                   ← its own Bash call
```

Commits are cheap and local. Accumulate commits, verify locally, push once: many commits and a single push per branch is the intended shape, not an accident.

**Phase 3 — integration**

```
11. git fetch
12. git merge --no-ff origin/main
13. build + test the MERGED tree               ← the step people skip
14. bash tools/checks/scan_semantic_collisions.sh
15. bash tools/checks/check_license_headers.sh
16. git push
```

Step 13 matters because required checks are **not strict**: a branch can be green against a base it was never built on. Step 14 catches what a textually-clean merge hides — two sessions minting the same ADR number in different files, or the same amendment heading. Step 15 catches licence terms that rode in with copied material.

**Phase 4 — opening**

```
17. gh pr create --base main --head "$BRANCH"
18. gh pr merge <n> --auto                     # ONLY when the branch is done
19. tools/gh/wait-merged.sh <n> &              # background; its exit is the callback
19b. tools/gh/pr-review-status.sh <n> --wait 30m &
```

Drop `--delete-branch`: the queue owns branch cleanup and `gh` rejects the flag.

**Phase 5 — landing**: the queue builds the head on current `main`, merges, and pushes. Return to `main` deliberately, on the notification — never let a background watcher move your tree.

**Phase 6 — after**

```
20. tools/gh/pr-sessions.sh /unresolved        # what still owes a reply
21. tools/gh/pr-review-status.sh <n>           # was it REALLY reviewed, and against which head
22. tools/gh/pr-reply.sh … <<'EOF'             # reply + resolve one thread; body on STDIN
```

Phase 6 is not optional and nothing reminds you: no red check, no blocked merge. A PR can and does merge with findings outstanding.

**Answering findings is a TASK, so it starts at Phase 1 like any other** — fetch, check where you are, cut a fresh branch. The findings arrived on a branch that is already merged and deleted, so whatever you are standing on is by definition the wrong place. Any code fix lands on a new branch; only the replies go to the old PR.

Never post a reply body through `gh api -f body="…"`: replies quote code, and a double-quoted body has its backticks command-substituted and its `$vars` expanded before `gh` sees them. `pr-reply.sh` takes the body on STDIN for exactly this reason — the same hazard as the heredoc rule for commit messages.

#### The tooling

`tools/gh/` scripts are the supported way to observe a PR; each carries its own `--help`, and each has a self-test beside it (`test_*.sh`) that must stay green.

| script | what it answers |
|---|---|
| `wait-merged.sh <n>` | blocks until the PR reaches a terminal state; run it in the **background**, its exit IS the callback |
| `pr-review-status.sh <n>` | was this PR *actually* reviewed, by whom, and against which head |
| `pr-sessions.sh` | which session owns which PR; `/unresolved` lists PRs still owing a reply |
| `pr-reply.sh` | reply to and resolve one review thread, body on STDIN |
| `actions-health.sh` | is Actions healthy enough to be worth spending a run on |

`wait-merged.sh` exit codes, because they arrive with no time to go reading:

| exit | verdict |
|---|---|
| 0 | MERGED — safe to return to `main` |
| 3 | CLOSED without merging — do NOT return to `main` |
| 5 | BLOCKED — a required check already failed, or the base conflicts |
| 6 | STALLED outside the PR — Actions degraded, or a REQUIRED check has no run to report it (lost webhook, or a filter excluding it) |
| 4 | watch expired, state genuinely unknown |
| 2 | usage error, or the PR could not be read repeatedly |

`BLOCKED` while checks are merely pending is the normal waiting state, not a verdict. `pr-review-status.sh` exit 5 means no review is coming at all — the head advanced past the last review and none was requested; it returns immediately rather than sitting out the timeout, so a fast `--wait` is an answer, not a failure.

`actions-health.sh` reports remaining allowance only when `INTERWEAVE_ACTIONS_INCLUDED_MINUTES` is set in `.claude/settings.json`. It is deliberately unset: this repository is public, so Actions minutes are not metered. Set it only if that changes, and never hardcode a plan size anywhere else.

#### When to open a NEW PR

**Never open a new PR while your current one is still open.** One PR at a time, per session. If the branch you are on has an open PR, the next piece of work is another commit on it. Corollary: documentation of a thing belongs in the PR that adds the thing.

With the current PR landed, open a new one when: it depends on something being *merged* rather than merely written; the current branch is already queued or merged; it touches a slow or flaky surface that would hold the rest hostage; urgency differs; or the batch has grown past comfortable review (~6–8 commits — a reason to stop adding and land, not to open a second PR alongside).

Not on that list: "different concerns", "different packages", "different root causes". Those are commit boundaries, satisfied by committing separately on the same branch.

#### Never race your own CI

**Arm `--auto` only when the branch is finished.** Arming is standing consent: it lands the PR at the FIRST moment every required check is green, not when you decide you are done. Arm while more commits are coming and the PR can merge before your next commit exists. Arming late costs nothing, so there is no trade to make.

### Concurrent sessions, worktrees, and subagent dispatch

Multiple sessions may work this repository in parallel, possibly on different hosts. **Isolation is required**, and the model is **one full `git clone` per session**. Sessions coordinate **only** through `origin`.

- **Your starting directory is yours exclusively.** Assume sole ownership: `git add -A` is safe, builds and tests are trustworthy. If you observe changes you did not make — a dirty tree at start, foreign edits or files — that is a launcher misconfiguration (two sessions sharing a checkout). **Stop and report it**; do not work around it by staging selectively or committing to `main`.
- **Never push to, rebase, or delete a branch another session created.** The `<host>/<clone>/…` prefix tells you whose it is. Same for their PRs: do not retarget, re-title, or merge them. Only answer review comments on PRs you opened.
- **Never resolve a review thread you have not actually addressed.** Resolving signals "this was handled"; a false resolve buries the finding with nothing left to catch it.

#### Subagent dispatch

**Dispatching agents is OPT-IN: fan out only when the user asks for it.** A task that merely looks parallelisable is not an invitation. The isolation contract governs **how** to dispatch, never **whether**.

When you do dispatch, **`isolation: "worktree"` and `model` are both required on EVERY call.** The `PreToolUse` hook in `.claude/settings.json` denies a dispatch missing either, and denies ahead of the premium-model prompt so an `ask` cannot wave a missing worktree through. **Forks (`subagent_type: "fork"`) are the only carve-out** — a fork continues this session rather than being a separate agent.

- **Both apply inside a `Workflow` script too, and NOTHING ENFORCES THEM THERE.** The hook matches the tool name `Agent`, so a workflow's `agent(prompt, opts)` calls never reach it, and both fields default the wrong way: omitting `model` makes the agent inherit the **session** model, omitting `isolation` leaves it working in the **session's clone**. Write both out on every call: `agent(prompt, { model: 'haiku', isolation: 'worktree' })`.
- **`opus` and `fable` are FORBIDDEN as subagent models** unless the user's prompt explicitly asks for that tier for that dispatch. "The task looks hard" is not authorisation; neither is "the session is already running that model". Choose the cheapest tier that can do the job: `haiku` for mechanical, well-specified work; `sonnet` for judgement work. Fan-out multiplies cost by the agent count — a large wave is a reason to drop a tier, not to keep the session's.
- **The agent's side of the contract**: it receives a worktree path, stays inside it, and **runs no git at all** — it does not create, enter, exit, or remove a worktree, and it never commits.
- **Only the session commits.** Collecting an agent's work is a **copy**, not a merge: read what it produced, apply it in the clone yourself, and commit there. The `worktree-agent-<id>` branch is a by-product of isolation, not a delivery mechanism; merging it would put commits in the history the session did not author.
- **`worktree.baseRef` is pinned to `"head"`** in `.claude/settings.json`. The default (`fresh`) branches agent worktrees from `origin/<default-branch>`, so the agent sees **none** of the unmerged task branch — exactly when fan-out is worth doing. Do not revert it, and check it before diagnosing a "blind" agent.
- **`head` is the committed HEAD, not your working tree — so COMMIT BEFORE YOU FAN OUT.** Uncommitted edits and untracked files are invisible to every agent, and an agent asked to extend work you have not committed silently reads the *previous* version and reports success against it.
- **Partition WRITES by file set, and name that set in each prompt.** Isolation made concurrent mutation safe; it did not make it collectable. Two agents that both rewrite the same file hand the session two divergent versions and no merge. Overlapping reads are free.
- **The harness opens the worktree; the session closes it whenever the agent did work.** The harness auto-removes only worktrees left unchanged, so every productive dispatch leaves one behind:
  ```
  git worktree list
  git worktree remove --force <path> && git branch -D worktree-agent-<id>
  git worktree prune
  ```
  `.claude/worktrees/` is gitignored because it sits inside the repository and each entry holds a `.git` file — without the ignore, `git add -A` stages it as an embedded repository and commits a broken gitlink.

### Always

- `git fetch` before any push or integrate — your `origin/main` goes stale.
- Orient before EVERY `fetch` / `checkout` / `pull`, not just at task start. The Phase 1 questions cost one command each and are what stop a reflexive `checkout` from moving you off work you had not committed.
- **Never move the tree while async work is in flight.** Background agents and watchers outlive the turn that started them; a `checkout` / `pull` / `reset` underneath them is an unguarded race.
- **Never force-push** unless the user explicitly asks; no history rewrites of published commits.
- Read `git log` before assuming a commit is yours.
- Keep commits coherent and reviewable.
- Preserve repository history when moving files (`git mv` when appropriate).
- Do not mix unrelated architecture changes into implementation commits.
- Do not commit generated build outputs, local runtime state, or secrets.
- Do not publish releases, or change remotes/repository settings, unless explicitly instructed.
- Before committing, inspect the complete staged diff, not only the files you remember editing.

### Commit shape

These rules shape **what a commit contains**. They are not about git hosting, and they apply whether or not the work ends in a PR.

#### Multi-fix prompts

When a single prompt asks for **more than one unrelated fix** (different files, different bugs, different ADRs, different concerns — not the natural sub-tasks of one feature), do not bundle them into a single commit. For each fix in turn:

1. implement only that one fix;
2. add or update only the tests directly related to it;
3. run the impacted tests; verify they pass;
4. create one commit scoped to that fix, with a message describing only it;
5. move to the next fix.

A multi-fix prompt produces N commits, not one. Related sub-tasks of the same fix — a change plus its test plus the ADR cross-reference it requires — belong in the same commit; the discriminator is whether they share a single root cause, ADR, or feature.

Do not bundle "while I'm here" cleanups into a fix commit. Note the drift and defer it, or handle it as its own follow-up commit.

#### Multi-package prompts

When a single prompt's work spans more than one package under `apps/` or `crates/`, do not bundle it into a single commit even when it is one coherent feature. One commit per package, each with its own tests run before it lands.

Shared contract or documentation edits that enable **one** package's commit may ride with it. Shared edits that enable **more than one** go into their own preceding commit, so each package commit depends on it cleanly. Order by the authority direction: neutral API contracts before the backends and clients that consume them (§4).

#### Debugging hygiene

When a bug hunt peels back several independent root causes, each gets its own commit — the multi-fix discriminator is root cause, not symptom. Squashing the chain loses bisectability and the diagnostic narrative.

Clean up before committing: diagnostic instrumentation added during the chase, throw-away fixtures, and commented-out hypotheses. Keep what is genuinely production signal — a warning on a real fallback path, a log on a previously silent swallow. Never push noise "to clean up later".

### Repository-wide change verification

For repository-wide changes, verify at minimum:

- `git status` is understood;
- Markdown relative links still resolve;
- YAML/config examples still parse;
- `tools/checks/check_guards_are_wired.sh` is clean — every guard is invoked by a workflow and has a self-test beside it, because one that runs nowhere passes silently-green;
- `tools/checks/validate_adr_index.sh` is clean — every ADR is template-conformant, indexed, and digested, and its amendment record is consistent;
- `tools/checks/scan_semantic_collisions.sh` is clean — no two branches minted the same ADR number or amendment heading;
- `tools/checks/check_license_headers.sh` is clean — no missing Apache-2.0 header on first-party source, no foreign licence terms;
- `tools/checks/validate_contracts.py` is clean — every wire schema is meta-valid, manifested both ways, and traceable to an ADR and a prose specification;
- `tools/checks/verify_fixture_vectors.py` is clean — every frozen vector recomputes from its declared algorithm;
- no forbidden production artifacts were introduced outside the active stage;
- `git fsck --full` passes before archive handoff when a full repository ZIP is requested.

## 10. Context loading map

Task-scoped context loads on demand. Do not paste it back into this file.

- **Reading or navigating ADRs** → the `adr-lookup` skill. Digest first: `architecture/adr/ADR-DIGEST.md` keyword table, then the matching entries, then the full ADR only when your change touches its substance.
- **Writing a new ADR, amending one, or propagating an ADR change** → the `adr-authoring` skill, with `architecture/adr/ADR-TEMPLATE.md` as the structure and ADR-0048 as the model.
- **Why an ADR changed** → `architecture/adr/history/`. Research only; the body already says what the decision is today.
- **Why a branch, PR, or merge step exists** → §9, plus `tools/gh/<script>.sh --help`; each script documents its own exit codes.
- **The construction order and what may be built next** → `architecture/roadmap/BOTTOM-UP-IMPLEMENTATION-PLAN.md` and ADR-0046 (§1, §3).

## 11. Working principle

Implement from the bottom up and make boundaries executable through tests. Do not let convenience at a higher layer weaken a lower-layer invariant.
