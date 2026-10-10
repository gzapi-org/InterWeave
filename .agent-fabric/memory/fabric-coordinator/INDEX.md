---
role: "fabric-coordinator"
class: index
description: "What fabric-coordinator knows and where it lives."
tier: 1
distilled_at: "2026-10-10"
---

# fabric-coordinator — knowledge index

A session is given the charter and brief (in the launch prompt),
and the project's remit with a pointer to this index (from the
session-start hook) — nothing below. Open a slice when its cue
matches what you are doing; a `workflow` slice says how a kind of
work is done here, so read the matching ones before that work.
Paths are relative to this working copy; `../agent-fabric/` is the
control plane checked out beside it.

## charter

- [`../agent-fabric/identities/roles/fabric-coordinator/charter.md`](../agent-fabric/identities/roles/fabric-coordinator/charter.md) — Owns the control plane: agent-fabric's role definitions, catalogue, routing policy, authority rules and the GZCoord protocol; the only role that changes what other roles are.

## brief

- [`../agent-fabric/identities/roles/fabric-coordinator/brief.md`](../agent-fabric/identities/roles/fabric-coordinator/brief.md) — How fabric-coordinator works day to day: the control plane's only writer — roles, routing, policy, the protocol, the launch — changed in small verified commits, distributed to every account, never project truth.

## domain

- [`../agent-fabric/memory/domains/fabric-coordinator/domain/apache-ossie.md`](../agent-fabric/memory/domains/fabric-coordinator/domain/apache-ossie.md) — Apache Ossie (ex Open Semantic Interchange, OSI) — vendor-neutral JSON/YAML spec for exchanging semantic models (datasets, relationships, metrics, ai_context) across BI, analytics and AI tools; what it is, its shape, its maturity
- [`../agent-fabric/memory/domains/fabric-coordinator/domain/apostrophe-breaks-quoted-python.md`](../agent-fabric/memory/domains/fabric-coordinator/domain/apostrophe-breaks-quoted-python.md) — An apostrophe in a comment inside a shell script's single-quoted python block silently ends the quote; bash -n passes and the failure surfaces as a runtime syntax error far from the edit
- [`../agent-fabric/memory/domains/fabric-coordinator/domain/autonat-client-crate-facts.md`](../agent-fabric/memory/domains/fabric-coordinator/domain/autonat-client-crate-facts.md) — What libp2p-autonat 0.15.0's v2 CLIENT actually does — measured, and contradicting several accepted documents
- [`../agent-fabric/memory/domains/fabric-coordinator/domain/deepseek-harness.md`](../agent-fabric/memory/domains/fabric-coordinator/domain/deepseek-harness.md) — DeepSeek Harness (dsh) — DeepSeek's open-source, everything-is-a-plugin agent harness on Cordis; what it is, its shape, what maps onto the fabric (runtime adapter, Agent Teams vs GZCoord, Agent Notes vs ADRs, credentials, approval)
- [`../agent-fabric/memory/domains/fabric-coordinator/domain/locale-suffixes.md`](../agent-fabric/memory/domains/fabric-coordinator/domain/locale-suffixes.md) — The fabric's locale suffixes: ge is Georgian (language-culture-ge, script Georgian), ru Russian; German would be de — never read ge as German
- [`../agent-fabric/memory/domains/fabric-coordinator/domain/minigraf.md`](../agent-fabric/memory/domains/fabric-coordinator/domain/minigraf.md) — Minigraf — embedded single-file bi-temporal graph database with Datalog (Rust; Python/Node/WASM/mobile bindings), pitched as graph memory for AI agents; what it is, its limits, what it could mean for the fabric's memory corpus
- [`../agent-fabric/memory/domains/fabric-coordinator/domain/slice-correction-attribution-writer-name.md`](../agent-fabric/memory/domains/fabric-coordinator/domain/slice-correction-attribution-writer-name.md) — Correction for memory/shared/domain-claude-code-attribution-reminder.md: the writer bootstrap.sh runs is runtime/claude-code/user-settings.py (attribution-off.py until 2026-09-20), and it also sets showThinkingSummaries and verbose
- [`../agent-fabric/memory/domains/fabric-coordinator/domain/software-factory-loops-graphs-harnesses.md`](../agent-fabric/memory/domains/fabric-coordinator/domain/software-factory-loops-graphs-harnesses.md) — Ivo Kund's "software factory" (2026-09-11) — planning and execution loops, a dependency graph, a harness, an outer loop; what it claims and where the fabric already matches
- [`../agent-fabric/memory/domains/fabric-coordinator/domain/yamux-silent-downgrade.md`](../agent-fabric/memory/domains/fabric-coordinator/domain/yamux-silent-downgrade.md) — Tuning libp2p yamux via any setter silently swaps the muxer to vulnerable yamux 0.12.1, and cargo-deny cannot see that advisory
- [`../agent-fabric/memory/shared/domain-claude-code-attribution-reminder.md`](../agent-fabric/memory/shared/domain-claude-code-attribution-reminder.md) — The attribution key is written by runtime/claude-code/user-settings.py, run from tools/fabric/bootstrap.py (shared)

## solution

- [`.agent-fabric/memory/fabric-coordinator/solution/claude-md-skill-split-after-stage-7.md`](.agent-fabric/memory/fabric-coordinator/solution/claude-md-skill-split-after-stage-7.md) — DONE 2026-08-28 (PR #53): CLAUDE.md §9 lifecycle moved to the pr-lifecycle skill, 28% cut. Seven review findings, all one shape — keep this before splitting CLAUDE.md again
- [`.agent-fabric/memory/fabric-coordinator/solution/connectivity-constructor-gated-off.md`](.agent-fabric/memory/fabric-coordinator/solution/connectivity-constructor-gated-off.md) — Owner ruled 2026-09-07 that the relay/autonat/dcutr constructor ships gated off and ClassGated<B> lands first, reversing the approved phase order
- [`.agent-fabric/memory/fabric-coordinator/solution/opus-review-hook-prompt-means-stale-session.md`](.agent-fabric/memory/fabric-coordinator/solution/opus-review-hook-prompt-means-stale-session.md) — If the opus review dispatch prompts for authorization, settings.json is fine — the session holds a stale in-memory hook; restart, don't edit
- [`.agent-fabric/memory/fabric-coordinator/solution/stage-11-pr85-pr86-rounds.md`](.agent-fabric/memory/fabric-coordinator/solution/stage-11-pr85-pr86-rounds.md) — The review rounds of #85 (the vendored AutoNAT client) and #86 (the review sweep of merged code) — what each late round found and the fix that introduced the next
- [`.agent-fabric/memory/fabric-coordinator/solution/stage-11-review-rounds.md`](.agent-fabric/memory/fabric-coordinator/solution/stage-11-review-rounds.md) — Stage 11's per-PR record — what #80–#86 each landed, how many review rounds, and the defect each late round found (a knob in the schema is not a knob in the crate; a self-test baseline that asked only for a non-zero exit)
- [`.agent-fabric/memory/fabric-coordinator/solution/stage6-review-retrospective.md`](.agent-fabric/memory/fabric-coordinator/solution/stage6-review-retrospective.md) — Stage 6 took 13 review rounds and 37 findings; 46% were contract-code mismatches and 35% were caused by the previous round's fix
- [`.agent-fabric/memory/fabric-coordinator/solution/stage8-review-retrospective.md`](.agent-fabric/memory/fabric-coordinator/solution/stage8-review-retrospective.md) — Stage 8 (endpoint directory) shipped in 2 PRs, ~22 automated-review findings over 15 rounds; the source-spoofing P1, the vacuous-test lesson, and what the review chained on
- [`.agent-fabric/memory/fabric-coordinator/solution/vendored-autonat-client.md`](.agent-fabric/memory/fabric-coordinator/solution/vendored-autonat-client.md) — Why libp2p-autonat is built from third_party/ with one patch, and what that obliges on every libp2p bump

## rationale

- [`.agent-fabric/memory/fabric-coordinator/rationale.md`](.agent-fabric/memory/fabric-coordinator/rationale.md) — cargo-mutants was measured against InterWeave on 2026-08-26 and dropped — the suite caught everything, at ~1.6h/night and three flags needed to avoid lying

## workflow

- [`.agent-fabric/memory/fabric-coordinator/workflow/anchor-inserts-after-not-before.md`](.agent-fabric/memory/fabric-coordinator/workflow/anchor-inserts-after-not-before.md) — When scripting an insertion into Rust source, anchor on the END of the preceding item — anchoring on the following item's declaration splits it from its doc comment
- [`.agent-fabric/memory/fabric-coordinator/workflow/announce-and-record-step-completion.md`](.agent-fabric/memory/fabric-coordinator/workflow/announce-and-record-step-completion.md) — When a canonical-plan step finishes, say so explicitly in the reply and update the stage-progress memory in the same turn
- [`.agent-fabric/memory/fabric-coordinator/workflow/ask-reviewers-to-generalise-findings.md`](.agent-fabric/memory/fabric-coordinator/workflow/ask-reviewers-to-generalise-findings.md) — When requesting a review, ask the reviewer to search for other instances of each finding's CLASS before reporting — the user pointed this out after rounds of one-instance-per-round
- [`.agent-fabric/memory/fabric-coordinator/workflow/assertions-that-cannot-fail.md`](.agent-fabric/memory/fabric-coordinator/workflow/assertions-that-cannot-fail.md) — A green mutation check proves the test catches THAT mutation, not that the assertion asserts what its name says — read the function the assertion calls
- [`.agent-fabric/memory/fabric-coordinator/workflow/audit-agent-ends-review-whack-a-mole.md`](.agent-fabric/memory/fabric-coordinator/workflow/audit-agent-ends-review-whack-a-mole.md) — When review rounds keep finding the same invariant at neighbouring sites, the user wants a fresh uncontexted agent to audit the whole class so one pass ends it — proven on PR #56 round 32
- [`.agent-fabric/memory/fabric-coordinator/workflow/commit-before-mutation-checking.md`](.agent-fabric/memory/fabric-coordinator/workflow/commit-before-mutation-checking.md) — Undoing a planted mutation with git checkout -- reverts the whole file to HEAD and silently destroys the uncommitted fix you were testing; commit first, or restore from a scratch copy
- [`.agent-fabric/memory/fabric-coordinator/workflow/extraction-does-not-cover-the-call-site.md`](.agent-fabric/memory/fabric-coordinator/workflow/extraction-does-not-cover-the-call-site.md) — Extracting an inline decision into a function makes the DECISION testable, never its call site; and "fixing" an unenforced-claim finding must not trade an honest disclaimer for a stronger false one
- [`.agent-fabric/memory/fabric-coordinator/workflow/fix-both-ends-of-a-key-pair.md`](.agent-fabric/memory/fabric-coordinator/workflow/fix-both-ends-of-a-key-pair.md) — Canonicalizing a lookup key at the write site but not the read site is worse than not canonicalizing at all; find every site before landing the first
- [`.agent-fabric/memory/fabric-coordinator/workflow/hysteresis-needs-counterexamples.md`](.agent-fabric/memory/fabric-coordinator/workflow/hysteresis-needs-counterexamples.md) — Three rounds on one hysteresis rule, each fix introducing the next defect; what would have caught it in one
- [`.agent-fabric/memory/fabric-coordinator/workflow/known-failing-masks-new-failures.md`](.agent-fabric/memory/fabric-coordinator/workflow/known-failing-masks-new-failures.md) — A known-failing local check hides new failures behind it; run the CI-equivalent command and suppress the known lint rather than reading past it
- [`.agent-fabric/memory/fabric-coordinator/workflow/measure-the-linter-before-asserting.md`](.agent-fabric/memory/fabric-coordinator/workflow/measure-the-linter-before-asserting.md) — A self-test fixture written from belief about what a linter reports failed three CI rounds; fetch the pinned tool into the scratchpad and measure first
- [`.agent-fabric/memory/fabric-coordinator/workflow/mutation-checks-need-the-formatted-text.md`](.agent-fabric/memory/fabric-coordinator/workflow/mutation-checks-need-the-formatted-text.md) — A scripted mutation that silently matches nothing reads as a passing mutation; patch after cargo fmt, against the formatted text, and confirm the patch applied
- [`.agent-fabric/memory/fabric-coordinator/workflow/reproduce-the-mechanism-not-a-lookalike.md`](.agent-fabric/memory/fabric-coordinator/workflow/reproduce-the-mechanism-not-a-lookalike.md) — A fixture that resembles a reviewer's scenario can pass vacuously; reproduce the exact mechanism they named and prove it with a mutation
- [`.agent-fabric/memory/fabric-coordinator/workflow/reviewer-declined-dispatch-opus.md`](.agent-fabric/memory/fabric-coordinator/workflow/reviewer-declined-dispatch-opus.md) — Run both reviewers, and READ the automated one's threads every round — a non-zero gate exit means "this head uncovered", never "no review exists"
- [`.agent-fabric/memory/fabric-coordinator/workflow/scope-review-briefs-to-the-diff.md`](.agent-fabric/memory/fabric-coordinator/workflow/scope-review-briefs-to-the-diff.md) — A review brief that restates the full PR range and asks for guard/count re-verification every round burns ~50k tokens per comment-only commit and manufactures fresh prose nits; scope it to the diff's hunks and forbid re-verifying settled…
- [`.agent-fabric/memory/fabric-coordinator/workflow/stage-closure-needs-approval.md`](.agent-fabric/memory/fabric-coordinator/workflow/stage-closure-needs-approval.md) — Never close a canonical stage without asking the owner first; Stage 10 named explicitly
- [`.agent-fabric/memory/fabric-coordinator/workflow/verification-pipelines-must-fail-loudly.md`](.agent-fabric/memory/fabric-coordinator/workflow/verification-pipelines-must-fail-loudly.md) — Never end a verification command in | head/tail — it masks the exit code and set -e; two broken states were committed (once pushed) because the pipeline's tail exited 0
- [`.agent-fabric/memory/fabric-coordinator/workflow/verify-subagent-coverage-not-just-findings.md`](.agent-fabric/memory/fabric-coordinator/workflow/verify-subagent-coverage-not-just-findings.md) — A subagent's 'all N findings covered' is not evidence N was right — check the union of results against the input list before acting

## threads

- [`.agent-fabric/memory/fabric-coordinator/threads.md`](.agent-fabric/memory/fabric-coordinator/threads.md) — Stage 11 current state — which PRs merged (#86, #85), what #84 waits on, the arming rule for a PR whose late rounds find only comment defects, and what is next

## recall

- [`../agent-fabric/identities/roles/fabric-coordinator/recall.md`](../agent-fabric/identities/roles/fabric-coordinator/recall.md) — How to ask the control plane about itself: status, routing, lint, migration records.
