# CLAUDE.md — InterWeave repository operating contract

This file is the working contract for Claude Code and other coding agents operating in the InterWeave repository. Read it before making changes.

## 1. Repository state

InterWeave is currently an **accepted architecture plus implementation/test skeleton**.

- `architecture/` is the normative design source.
- `apps/`, `crates/`, `tests/`, `fixtures/`, `test-data/`, `spikes/`, `packaging/`, and `xtask/` are tracked landing zones created by ADR-0045.
- `tools/` is repository tooling — PR/review scripts and tree checks — not an implementation landing zone. It is live now and not gated by stage discipline. Each script has a self-test beside it (`test_*.sh`) that must stay green.
- `.claude/` is committed shared agent configuration: `settings.json` and `statusline.sh` (§9), plus `skills/` — task-scoped procedures loaded on demand, see §10. Only `settings.local.json` and `CLAUDE.local.md` are per-developer and gitignored.
- Stages 0-9 are **complete** and **Stage 10 is open**. Read `[workspace].members` for the current roster and `[workspace.metadata.interweave].status` for the open stage rather than trusting either written here. **What each completed stage proved is in its `Met.` block in the canonical plan** — including, for Stages 6 through 9, the clauses met by scope and the limits their tests cannot reach. Read the block before extending that stage; it is the record, and this file does not mirror it. Stage 9's block matters more than most: it records that the mDNS multicast MECHANISM was never built, so the stage proved the provider and not LAN discovery.
- Three facts about the built code are **not** derivable from it, and are recorded here because nothing else would say them at the moment they matter. **The reservation map's waiter ACCOUNTING is inert today and must NOT be removed as dead weight** — SPIKE-002/A11 measured the unbounded version as a memory-exhaustion vector, 40 copies attaching 39 waiters with zero refusals, and it binds in every stage. **The Stage 6 P1 about binding the source endpoint to the caller's lease was carried to Stage 8 by an explicit decision on PR #38 and closed there** — a direct send now takes the `EndpointLease` `claim_endpoint` returns, and `EndpointRegistry::holds_lease` verifies its epoch against the live lease, so a caller sends only as an endpoint it actually claimed (the plan's Stage 8 `Met.` block records this). And **broadcast and direct must remain independently functional and must never substitute for each other**, which is a standing constraint rather than a stage's exit gate.
- Stage 10 is open: Kademlia, activating `crates/api/kademlia-control-api` and `crates/discovery/kademlia`, with the Swarm-owned driver in `crates/transport/libp2p`. Stage 9 activated `crates/discovery/{static,cache,mdns}`. The Stage-1 contract crates under `crates/api/` remain types and validation only — no I/O, no runtime, no backend — and the Stage-2 crates remain pure state machines.
- **Stage 10 had a second prerequisite beside SPIKE-003 — the capability-observation mapping — and it is CLOSED (2026-08-30).** Both are closed; neither blocks the stage. The mapping is `kademlia-integration.md` §7: a stored observation is `(protocol_family, wire_major, network_hash, role)` while a `ProtocolObservation` carries one `protocol_id`, and the four are encoded AS the derived server protocol string `/interweave/kad/<wire_major>.0.0/<network_hash>` — `role = server` implied by presence, minor and patch always zero. `PeerCache::candidates` fills the field and `add_hint` parses the exact grammar back, both round-tripped against the frozen namespace fixture. What is worth carrying forward is the reason the deferral was taken seriously: a targeted lookup built on an empty observation set does not fail loudly, it reads as "no peer supports this" and silently degrades to no targeting.
- **SPIKE-003 is closed (2026-08-30): PASS FOR THE STAGE, and it does NOT close ADR-0034's v1 release gate** — server-mode reachability evidence is not consumed (AutoNAT and Relay are absent from the feature list) and single-path capture is not shown to be reduced (measured against controls; no capture occurred, so the comparison cannot speak for the option). Implementing Stage 10 is unlocked; shipping configured entries default-enabled is not. Its findings bind the stage rather than merely informing it; the record is `spikes/spike-003/README.md` and the verdict is in `architecture/roadmap/SPIKES.md`. The one that changes the ORDER of the work: **Stage 10 cannot begin by enabling the feature.** The production `OutboundAdmission` refuses every dial carrying no root admission ticket, and every Kademlia query dial carries none — so turning `kad` on without first extending the gate to admit a behaviour-originated dial *through* `PolicySnapshot::admit` under `DialOrigin::KademliaQuery` yields a subsystem whose every query dies at the first hop it lacks a connection for, silently, because a refused behaviour dial surfaces as an ordinary dial failure. Seventeen findings in total, five saying the gate cannot be written the obvious way and three naming API changes the production crates need. Two that a reader of the design would not predict: a routing insertion starts one query nobody asked for and it dials, so policy installed after seeding is installed after the dial it meant to govern; and under `BucketInserts::Manual` a seed node routes NOBODY, because inbound connections insert nothing — the admission pipeline in `kademlia-integration.md` §7 reads as an outbound story and a bootstrap node lives on the other direction. What the spike did NOT establish is stated in its record and must not be read out of its silence, above all that **server-mode reachability evidence is not validated**: AutoNAT and Relay are absent from the feature list, so SPIKE-004 is where that arrives.
- The toolchain is pinned in `rust-toolchain.toml`; edition, MSRV, lints, shared dependency versions, and the release profile are declared once in the root `Cargo.toml` and inherited.
- Production Rust exists and grows one stage at a time; `apps/` and `packaging/` are still empty, so there is no binary, installer, or service unit yet.
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

**Start at the digest.** `architecture/adr/ADR-DIGEST.md` is the cheapest correct way to find which decisions govern a change, and the `adr-lookup` skill is the procedure for using it. What belongs HERE is only its standing: the digest is a navigation aid, not an authority. It sits below everything in the list above, on any discrepancy the ADR wins and the digest is what gets fixed, and no normative constant is ever read from it — limits, wire formats and vectors come from the contracts and `fixtures/`.

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

#### A comment that claims an invariant owes a test

**Every comment containing "never", "only", "bounded", "exactly", or "fails closed" must point to a test that would fail if that statement stopped being true.**

A comment is not enforcement. Writing the reasoning down is the step that *feels* like doing the work, which is exactly why it substitutes for it so easily: the claim reads as settled, review reads it as settled, and nothing anywhere fails when it stops being true. This repository has already shipped a helper whose own documentation explained that a caller who skipped it would get "a gate that looks like it is working" — and that helper was called by nothing.

So the rule is mechanical, and the check is mechanical too:

- Find the test that fails if the sentence becomes false. Not a test of the same function — a test of **that claim**.
- If there is no such test, either write it or delete the claim. A weaker true comment beats a strong unenforced one.
- **Break the code and watch the test fail.** A test written from the same belief as the comment agrees with the comment for free; the mutation is what proves the test is load-bearing. Every one of the recurring defects here passed its tests, because the test fed the function the shape the author already had in mind.
- Feed the test what the **caller actually holds**, not what the function was designed for. `source_bucket` was correct for every input its tests supplied and wrong for the string a listener hands over, three separate times.

The words are a trigger, not the whole set — an invariant phrased without them owes the same test. The list exists so the rule can be applied without judgement, on sight.

#### Prose that describes behaviour you changed is part of the change

**When behaviour changes, search the tree for the OLD behaviour's own words and fix every place that still asserts them — before committing, not after a reviewer names one.**

The rule exists because the failure is not carelessness in the reasoning. It is that the reasoning is correct in the file you are looking at, and its counterpart lives somewhere you are not looking. One change, one edit, one file — and the pair goes stale silently, because nothing compiles prose.

The instances, so the shape is recognisable rather than abstract: a schema's `x-contract.status` flipped without its manifest entry; a stage's `Met.` block claiming evidence its tests did not have; a required-test bullet pointing at an amendment that had been withdrawn two commits earlier; a `README` status paragraph left behind by the very commit that changed the plan it summarises; an ADR amendment written in the present tense about a defect the next commit fixed; a comment opening "A WAITER IS ANSWERED, not dropped" directly above a branch that had just been made to drop deliberately.

Every one was caught in review. None was caught by re-reading the diff, because the diff does not contain the file that was not edited.

What to actually do, in order:

- **Grep for the old behaviour's distinctive words**, not for the filename you edited. `grep -rn 'is answered \`overloaded\`' architecture/ crates/` found two stale claims a review had not named.
- **Include comments, ADR history notes, `README`/`IMPLEMENTATION` status prose, contract text and plan sections.** A comment is prose that ships; an ADR history note written in the present tense becomes false the moment the behaviour it describes is fixed, and it is the one document a future reader trusts to say what was true *then*.
- **Prefer past tense for a defect an amendment responds to.** "As this gap was found, X was answered Y" stays true forever; "X is answered Y" is false as soon as it is fixed — often in the same commit series.
- **A pair is not always two files.** The waiter comment and the code it described were forty lines apart in one file.

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

Amending an ADR is a three-part record (ADR-0048), and the `adr-authoring` skill carries the mechanics. The part that is a judgement rather than a procedure stays here: **a change of substance is not an amendment, it is a new superseding ADR, and the test is whether a reader who followed the old text would now be wrong.**

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

### The dependency policy is enforced

`deny.toml` is the accepted policy and `tools/checks/check_dependencies.sh` enforces it. The licence list is an **allow-list containing exactly the terms the current graph resolves to** — a deny-list only stops what someone thought to name, and an aspirational entry lets the next dependency in without anyone deciding.

Adding a dependency whose licence is not already listed therefore fails the check. That is the intended cost: widening the list is a licensing decision, and it should arrive as a commit with a sentence about why those terms are acceptable for an Apache-2.0 project — not as a one-word edit made to get CI green.

The same file forbids git dependencies and any registry other than crates.io. A git dependency has no version, no yank mechanism, and no advisory database, so it is outside every other control in this section.

#### The advisory check sees RustSec, and GitHub sees more

`cargo-deny` resolves advisories against the RustSec database. A
vulnerability published only as a GHSA has no RUSTSEC id, so the check
cannot see it and reports clean — accurately, for the question it asks.
Treat Dependabot as a second, non-overlapping source rather than a
duplicate of the dependency check.

That gap is live: **`yamux`** has no RustSec advisory, and every `Config`
tuning setter silently moves the muxer onto a version with a remote-panic
DoS. Bounding stream counts is exactly what §6 pushes toward, so the
natural next change reintroduces it with every check green.
`tools/checks/check_yamux_muxer.sh` is the guard, because `cargo-deny`
structurally cannot be; its `--help` carries the mechanism, the advisory
id, and why banning the version would not work.

### Licence headers are checked

Every first-party source file carries an `SPDX-License-Identifier:
Apache-2.0` header in its opening lines, and
`tools/checks/check_license_headers.sh` enforces that plus the absence of
foreign licence terms anywhere in the tracked or about-to-be-committed
tree.

The decision behind it: code copied in from a differently-licensed source
keeps its own terms until the copyright holder relicenses it, and a public
Apache-2.0 tree is where that goes unnoticed. Genuinely third-party
material is therefore an **exemption with recorded provenance** in
`tools/checks/license_exempt.txt`, never a silent relabel.

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
- **Check what EVERY open PR touches before choosing this task — other sessions' as well as your own.** `tools/gh/pr-sessions.sh /all /OPEN` lists them; `gh pr view <n> --json files` says what each one holds. Steps 2 and 3 below do not answer this: step 2 asks only about the branch you are standing on, and step 3 sees `origin/main`, where work sitting in an unmerged PR by definition is not. Partition by FILE SET, not by intent: two open PRs touching the same file will conflict, and the second to land pays for it with an unplanned rebase and a re-review of a tree neither review saw. A refactor of a file is a hard exclusion — nothing else may touch that file until it lands. And a task that depends on another being **merged** waits for the merge, not for it to be written; building on an unmerged branch means duplicating its commits or standing on a base nobody reviewed. Whose PR it is changes the RESPONSE, never the check: around your own you re-plan freely, around another session's you partition or coordinate — never push to its branch, rebase it, or answer its reviews. These are start-of-task decisions, which is why they are here and not in the lifecycle skill.

```
1. git rev-parse --abbrev-ref HEAD            # where am I?
2. gh pr list --head "$BRANCH" --state all    # is this branch spoken for?
3. git fetch && git log origin/main           # has someone already done this?
4. git checkout main && git reset --hard origin/main
5. git checkout -b <host>/<clone>/<type>/<short-desc>
```

Steps 4–5 are **unconditional**: the step-2 lookup only speaks when a PR already exists.

**Step 4 destroys uncommitted work, so the rule that guards it is here and
not in a skill.** Your starting directory is yours exclusively: assume sole
ownership, and `git add -A`, builds and tests are all trustworthy. If you
observe changes you did not make — a dirty tree at start, foreign edits, or
files you did not create — that is a launcher misconfiguration, two
sessions sharing one checkout. **Stop and report it. Do not reset, do not
stage selectively, do not commit to `main` to get clear of it.** A
`reset --hard` in that state erases another session's uncommitted work, and
nothing afterwards can tell you it happened.

### Nothing prompts before code lands — the discipline below is all there is

`.claude/settings.json` carries an empty `"ask"`. `git push`, `gh pr merge`,
and the GraphQL mutations that do the same job all run without a
confirmation prompt. **This is deliberate**, and it was chosen knowing the
cost: a session idling through review rounds for a human who had already
approved the work spends most of its time waiting.

So read the rest of this section as the whole of the protection, not as a
reminder attached to one.

The chain that lands code is `gh pr merge --auto` → checks → merge queue →
`main`. **Arming is standing consent**: it lands the PR at the first moment
every required check goes green, whether or not anyone is still looking,
and whether or not a review ever arrived. Nothing asks first, nothing
blocks it, and no red check appears afterwards to say it was premature.

What that leaves load-bearing is stated in full below: arm only when the
branch is finished, wait for a security-boundary change's review on the
current head, and carry zero unresolved P1/P2 findings into a stage
closure.

`git push` never landed anything even when it did prompt: `main` is
protected, a pull request is required, and an unarmed PR sits indefinitely.
Push early and often — a review reads what is on the remote and nothing
else.

A session cannot see its own permission prompts, so do not try to infer any
of this from the transcript: an approved action and an auto-allowed one
produce an identical tool result. The only way to know what is gated is to
read `.claude/settings.json`.

### Merge / PR discipline

`main` is protected by the `main protection` ruleset: pull request required, direct pushes and force-pushes blocked, branch deletion blocked, **merge commits only** (squash and rebase disabled), and a **merge queue** (`ALLGREEN`, merge method `MERGE`). Required approving reviews are 0 and unresolved review threads do not block — so **the merge is not evidence that anything was reviewed**.

>  **CI exists and gates `main`** — `.github/workflows/ci.yml` runs fmt, clippy, the workspace tests, every tree check, and every self-test on `pull_request`, `merge_group`, and pushes to `main`. It reports three contexts, which are the job `name:` values verbatim: **`rust`**, **`tree checks`**, and **`tool self-tests`**. All three are in the ruleset's `required_status_checks`, so the queue gates correctness and not merely ordering. `tools/checks/check_required_contexts.sh` keeps this paragraph and the workflow in agreement; the ruleset itself needs admin API access and is checked by hand. The policy is **non-strict**: a branch need not be up to date with `main` to merge, which is why folding `origin/main` in and re-testing locally (Phase 3) is still on you rather than on the platform.
>
> A job's `name:` *is* its required-check context, so renaming a job silently un-gates `main` — the ruleset goes on requiring a context nothing reports, and the queue waits forever. Rename a job only together with the ruleset.
>
> `merge_group` in that workflow is equally load-bearing: the queue builds its own ref, so a workflow that triggers only on `pull_request` never reports for that build and the queue hangs on a check that will never arrive.

**Commit shape and PR shape are different questions.** The multi-fix and multi-package rules below govern COMMITS — one per root cause, one per package. Bisectability, revertability, and reviewability all live at the commit level and are unaffected by batching several commits into one PR.

#### The rest of the lifecycle is a skill

Phases 2–6 — working, integration, opening, landing, and the follow-up
that nothing reminds you to do — plus the `tools/gh/` tooling, the rules
for opening a second PR alongside an open one, concurrent-session
isolation and subagent dispatch, all live in the **`pr-lifecycle` skill**
(§10). Load it when you are about to commit, push, open a PR, arm a merge,
answer findings, **or dispatch a subagent** — dispatch is a trigger because
`worktree.baseRef` is `head`, so an agent reads your last COMMIT and never
your working tree, and the rule that follows from that is in the skill.

What stays here is what a session needs *before* it knows that skill
applies: where a branch comes from (above), that nothing prompts before
code lands (above), and the review gate below.

#### Never race your own CI

**Arm `--auto` only when the branch is finished.** Arming is standing
consent: it lands the PR at the FIRST moment every required check is green,
not when you decide you are done. Arm while more commits are coming and the
PR can merge before your next commit exists. Arming late costs nothing, so
there is no trade to make.

#### A security-boundary change waits for its review

**Do not arm `--auto` on a change to a security boundary until the
automated review has reported on the current head.** Green checks are not a
review: §9 already says the merge is not evidence that anything was
reviewed, and the queue lands a PR the moment the last check passes.

The gap is not theoretical and it is measured in seconds. PR #28 merged at
06:56:03 UTC and the review that found a P1 in it arrived at 06:56:15 —
twelve seconds later, against a branch that no longer existed. The finding
then cost a fresh branch, a second PR, and a reply on a merged thread, all
to land a two-line fix that would have been one more commit had anyone
waited.

A **security boundary** here means identity and key handling, trust policy,
wire or configuration parsing, persistence, admission and resource
accounting, and anything cryptographic. Documentation and mechanical
changes are not on that list and should not wait.

The mechanics, and the step that is easy to miss: **a push does not
re-trigger automated review, so waiting for one you never asked for is
waiting forever.** Open the PR, request a review explicitly, and only then
run `tools/gh/pr-review-status.sh <n> --wait --automated-only` in the
background, arming once it reports a review of the current head.

**`--automated-only` is part of the instruction, not a refinement of it —
the bare command does not satisfy this gate.** Without the flag, coverage
is any review by anyone who is not the PR author, and this repository is
public: any GitHub user can submit a review object on an open PR. One
drive-by review carrying the current head exits 0 and arms the merge this
rule exists to hold open. The flag restricts coverage to the recognised
reviewer. The script's `--help` explains the rest.

**A clean review leaves no review object.** When the reviewer finds nothing
it posts an ordinary issue comment naming what it looked at, and creates no
review; `pr-review-status.sh` reads those as coverage. A review that
arrives while you are still committing is a review of the wrong tree, which
is a reason to arm late rather than a reason not to wait.

Zero unresolved P1 or P2 findings is also a precondition for declaring a
stage complete. Nothing enforces that; it is the same class of obligation
as the follow-up phase, which no red check announces either.

#### When the reviewer declines, dispatch one — this is the exception

`@codex review` can come back **"You have reached your Codex usage limits
for code reviews."** That is not coverage, and `pr-review-status.sh`
counts the refusal as "already answered" — so the gate above reads as
satisfied while nothing has been reviewed. It has already let two PRs
merge with their final heads unreviewed (#58 and #59; #59 merged four
hours after the refusal, carrying a 265-line rewrite of the conformance
suite).

So when the reviewer declines for usage limits, **dispatch a subagent to
do the review instead**. The rules that normally govern dispatch are
relaxed here, deliberately, and only here:

- **Reviewing is an exception to the opt-in rule.** §9 and the
  `pr-lifecycle` skill say fan-out happens only when the user asks. A
  review after a declined request does not need asking — the alternative
  is landing unreviewed code.
- **`model: "opus"`**, per the standing rule below — no per-dispatch
  authorisation needed.
- **One agent per PR, with NO context from the session.** Pass the PR's
  tree and diff and nothing else. An agent told what the author expects
  confirms it; the whole value is that it does not know.
- **No worktree — a review reads the session tree directly.** Isolation
  exists to keep an agent's WRITES out of the clone, and a review writes
  nothing, so a worktree buys nothing here and costs something real:
  `worktree.baseRef` is `head`, so an isolation worktree shows the last
  COMMIT and a reviewer inside one cannot see uncommitted work at all.
  Omit `isolation`, give the agent the repository path, and tell it the
  tree is read-only — the instruction is what holds, as it already does
  for "run no git" and "never commit".
- **Name it, or it gets neither exemption.** The hook exempts a dispatch
  whose `description` BEGINS with `review` or `re-review` AND whose
  model is `opus`. Both halves are load-bearing, and a review found that
  out: matching `review` anywhere let `Address review feedback` through
  — a WRITING dispatch that would then have run unisolated in the
  session clone — and without the model condition a `sonnet` or `fable`
  dispatch could take the exemption and evade the rules beside it.
- **The review goes ON THE PR, not into the transcript.** The agent's
  report is not the deliverable — post the findings to the pull request,
  fix them, and answer there. A finding that lives only in a session is
  a finding nobody can audit, and the thread is what makes the fix
  checkable against the claim.
- **A CLEAN review is posted too.** Say what was read and that nothing
  was found. This is the case the rule most needs, and the easiest to
  skip: there is no finding to write up, so the natural move is to arm
  the merge and move on. But `pr-review-status.sh` has already counted
  the usage-limit refusal as an answered request, so the gate reads as
  satisfied — and with nothing on the PR, the record shows a review
  that was declined and no evidence any other one happened. A clean
  comment is the only thing separating "reviewed, nothing found" from
  "never reviewed".

Everything else still applies: the findings are input rather than
verdicts, a disagreement is stated with its reasoning rather than
silently skipped, and a thread is resolved only when the work it names
is done.

#### A review runs on `opus`, and does not ask

**Every subagent doing a code review uses `model: "opus"`.** This is a
STANDING authorisation, not a per-dispatch one — it satisfies the
premium-tier rule's "unless the user's prompt explicitly asks for that
tier" clause once, here, for the whole class. Do not ask again, and do
not fall back to `sonnet` because a particular review looks small.

It applies to any review dispatch, not only the declined-reviewer path
above: a review requested directly, a second opinion on a change already
reviewed, an audit of merged code. If the job is *reviewing*, the tier
is settled.

The reasoning is the asymmetry. Everywhere else, the cheapest tier that
can do the job is right because a weaker answer costs a retry. A review
is the last thing between a defect and `main`, and its failure mode is
not a retry — it is a green PR that merges. The defects this repository
has actually shipped were found by review, and the ones review missed
became P1s discovered rounds later. Tokens are the cheaper side of that
trade by a wide margin.

The other dispatch rules are unaffected: cheapest tier still governs
extraction, search, pattern-following edits and everything else, and
`fable` remains authorised only when asked for by name.

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

**`cargo xtask checks` runs every tree check; `cargo xtask ci` adds fmt,
clippy, the workspace tests and every self-test.** Nothing short-circuits,
so one invocation reports everything that is wrong. Run it before
committing a repository-wide change, and read each script's `--help` for
what it actually asserts — they are not restated here, because a list of
one-line summaries goes stale while the scripts do not.

CI still invokes the scripts by name rather than through `xtask`. That is
deliberate: it is what makes each one visible to
`tools/checks/check_guards_are_wired.sh`, which fails on a guard that runs
nowhere.

Two things `xtask` cannot tell you:

- **`cargo-deny` reads RustSec and nothing else**, so a GitHub-only
  advisory is invisible to it and `check_dependencies.sh` passes
  truthfully while Dependabot reports a high. That gap is real today — see
  the `yamux` note in §8.
- **`git fsck --full`** before archive handoff, when a full repository ZIP
  is requested.

And the one thing no check covers: **no forbidden production artifacts
outside the active stage** (§3).

## 10. Context loading map

Task-scoped context loads on demand. Do not paste it back into this file.

- **Reading or navigating ADRs** → the `adr-lookup` skill. Digest first: `architecture/adr/ADR-DIGEST.md` keyword table, then the matching entries, then the full ADR only when your change touches its substance.
- **Writing a new ADR, amending one, or propagating an ADR change** → the `adr-authoring` skill, with `architecture/adr/ADR-TEMPLATE.md` as the structure and ADR-0048 as the model.
- **Why an ADR changed** → `architecture/adr/history/`. Research only; the body already says what the decision is today.
- **Committing, pushing, opening a PR, arming a merge, answering findings, or dispatching a subagent** → the `pr-lifecycle` skill. §9 keeps only what you need before that skill applies: where a branch comes from, that nothing prompts before code lands, and the security-boundary review gate.
- **What a `tools/gh/` script does, its flags and exit codes** → `tools/gh/<script>.sh --help`. Each ships its own, and a table written elsewhere goes stale while the script does not.
- **The construction order and what may be built next** → `architecture/roadmap/BOTTOM-UP-IMPLEMENTATION-PLAN.md` and ADR-0046 (§1, §3).

## 11. Working principle

Implement from the bottom up and make boundaries executable through tests. Do not let convenience at a higher layer weaken a lower-layer invariant.
