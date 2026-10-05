---
role: "devex-tooling"
class: index
description: "What devex-tooling knows and where it lives."
tier: 1
distilled_at: "2026-10-05"
---

# devex-tooling — knowledge index

A session is given the charter and brief (in the launch prompt),
and the project's remit with a pointer to this index (from the
session-start hook) — nothing below. Open a slice when its cue
matches what you are doing; a `workflow` slice says how a kind of
work is done here, so read the matching ones before that work.
Paths are relative to this working copy; `../agent-fabric/` is the
control plane checked out beside it.

## charter

- [`../agent-fabric/identities/roles/devex-tooling/charter.md`](../agent-fabric/identities/roles/devex-tooling/charter.md) — The machinery everyone works inside: the agent harness, the forge, repo tooling, branch and commit discipline, the local development stack.

## brief

- [`../agent-fabric/identities/roles/devex-tooling/brief.md`](../agent-fabric/identities/roles/devex-tooling/brief.md) — How devex-tooling works day to day, in any project: keeps the machinery every other session works inside honest — the local stack, the forge tooling, the guards, the harness wiring, the host docs.

## domain

- [`../agent-fabric/memory/domains/devex-tooling/domain/amend-records-only-staged-changes.md`](../agent-fabric/memory/domains/devex-tooling/domain/amend-records-only-staged-changes.md) — git commit --amend -F - without -a re-records the message only; an edit made after the commit stays unstaged — check git status before pushing an amend
- [`../agent-fabric/memory/domains/devex-tooling/domain/aot-snapshot-byte-find-needs-a-positive-control.md`](../agent-fabric/memory/domains/devex-tooling/domain/aot-snapshot-byte-find-needs-a-positive-control.md) — A byte-find on a Flutter release snapshot (libapp.so) passes on an --obfuscate build because the names are gone; require a control string that only an unobfuscated snapshot carries.
- [`../agent-fabric/memory/domains/devex-tooling/domain/bash-scripting-pitfalls.md`](../agent-fabric/memory/domains/devex-tooling/domain/bash-scripting-pitfalls.md) — bash's command_not_found_handle runs in a subshell (5.3+) — a counter-based self-test guard built on it is silently vacuous
- [`../agent-fabric/memory/domains/devex-tooling/domain/cargo-machete-dev-deps-and-silent-fallback.md`](../agent-fabric/memory/domains/devex-tooling/domain/cargo-machete-dev-deps-and-silent-fallback.md) — cargo-machete 0.9.2: default mode never reads [dev-dependencies]; --with-metadata does, but exits 0 silently when cargo metadata fails — resolve metadata yourself first
- [`../agent-fabric/memory/domains/devex-tooling/domain/claude-code-bash-permission-matcher.md`](../agent-fabric/memory/domains/devex-tooling/domain/claude-code-bash-permission-matcher.md) — Claude Code's Bash permission matcher is a text-pattern system with specific, repeatedly-rediscovered bypass and false-safety classes
- [`../agent-fabric/memory/domains/devex-tooling/domain/claude-github-app-billing.md`](../agent-fabric/memory/domains/devex-tooling/domain/claude-github-app-billing.md) — Claude/Anthropic GitHub Actions installed via /install-github-app bill the subscription
- [`../agent-fabric/memory/domains/devex-tooling/domain/fabric-lint-is-not-in-run-sh.md`](../agent-fabric/memory/domains/devex-tooling/domain/fabric-lint-is-not-in-run-sh.md) — agent-fabric tests/run.sh does not run tools/fabric/lint.py; a generic file naming a managed project (gzapp #N) passes the suite and is refused by lint
- [`../agent-fabric/memory/domains/devex-tooling/domain/fast-clock-through-bash-env.md`](../agent-fabric/memory/domains/devex-tooling/domain/fast-clock-through-bash-env.md) — Speed up a bash suite whose script under test naps on a wall-clock SECONDS deadline — source a sleep() through BASH_ENV that ages SECONDS, keep real latency on `command sleep`, and prove the clock is live with a control case.
- [`../agent-fabric/memory/domains/devex-tooling/domain/find-is-bfs-not-gnu-findutils.md`](../agent-fabric/memory/domains/devex-tooling/domain/find-is-bfs-not-gnu-findutils.md) — `find` on develop-qzapp is bfs, which rejects relative timestamps — and the error reads as "zero results" whenever stderr is discarded.
- [`../agent-fabric/memory/domains/devex-tooling/domain/gh-pr-merge-strategy-flag-on-a-queued-repo.md`](../agent-fabric/memory/domains/devex-tooling/domain/gh-pr-merge-strategy-flag-on-a-queued-repo.md) — `gh pr merge --auto --merge` on gzapp prints what looks like a refusal, leaves autoMergeRequest null, yet DOES enqueue the PR.
- [`../agent-fabric/memory/domains/devex-tooling/domain/github-actions-billing.md`](../agent-fabric/memory/domains/devex-tooling/domain/github-actions-billing.md) — A nonzero Actions netAmount (filtered to the Minutes SKU) means overage IS being purchased and runners ARE still dispatching — not that the allowance is spent
- [`../agent-fabric/memory/domains/devex-tooling/domain/github-actions-reusable-workflows.md`](../agent-fabric/memory/domains/devex-tooling/domain/github-actions-reusable-workflows.md) — Inside a reusable workflow, github.event_name reflects the CALLING workflow's event
- [`../agent-fabric/memory/domains/devex-tooling/domain/github-runner-unprivileged-userns.md`](../agent-fabric/memory/domains/devex-tooling/domain/github-runner-unprivileged-userns.md) — ubuntu-latest (24.04) blocks `unshare -r` via AppArmor; one sysctl lifts it, and a dummy link inside the netns needs no modprobe — measured 2026-09-25
- [`../agent-fabric/memory/domains/devex-tooling/domain/grep-q-pipefail-silent-pass.md`](../agent-fabric/memory/domains/devex-tooling/domain/grep-q-pipefail-silent-pass.md) — In a guard under set -o pipefail, never end a pipe in grep -q -- its early exit SIGPIPEs the writer and a match reads as a miss
- [`../agent-fabric/memory/domains/devex-tooling/domain/make-shell-arguments-cannot-hold-unbalanced-parens.md`](../agent-fabric/memory/domains/devex-tooling/domain/make-shell-arguments-cannot-hold-unbalanced-parens.md) — A `case … *)` (or any unbalanced `)`) inside a Makefile $(shell …) ends the call early and the rest of the text becomes the value; and a value substituted into a $(shell bash -c '…') string is shell text at parse time on every make.
- [`../agent-fabric/memory/domains/devex-tooling/domain/npm-ci-scripts.md`](../agent-fabric/memory/domains/devex-tooling/domain/npm-ci-scripts.md) — npm run --if-present silently no-ops — dangerous for a script that's the only check of something
- [`../agent-fabric/memory/domains/devex-tooling/domain/podman-cli-quirks.md`](../agent-fabric/memory/domains/devex-tooling/domain/podman-cli-quirks.md) — Podman has non-obvious quirks in non-interactive local-dev workflows
- [`../agent-fabric/memory/domains/devex-tooling/domain/podman-lock-collision-deadlocks-start-after-reboot.md`](../agent-fabric/memory/domains/devex-tooling/domain/podman-lock-collision-deadlocks-start-after-reboot.md) — after a VM reboot, `podman start` (and then every podman ps/stats) hung on a futex — the compose pod and a volume shared lock 0; `podman system renumber` fixes it
- [`../agent-fabric/memory/domains/devex-tooling/domain/postgres-image-layout.md`](../agent-fabric/memory/domains/devex-tooling/domain/postgres-image-layout.md) — Postgres 18+ images moved the data directory under a version subdirectory
- [`../agent-fabric/memory/domains/devex-tooling/domain/pretooluse-guard-cannot-enumerate-deny.md`](../agent-fabric/memory/domains/devex-tooling/domain/pretooluse-guard-cannot-enumerate-deny.md) — A custom PreToolUse hook that DENIES known-bad text shapes and stays silent otherwise fails open, not closed — it needs the opposite polarity
- [`../agent-fabric/memory/domains/devex-tooling/domain/sudo-i-leaves-dollar-live-in-the-target-shell.md`](../agent-fabric/memory/domains/devex-tooling/domain/sudo-i-leaves-dollar-live-in-the-target-shell.md) — sudo -i with a command re-escapes the argv for the target login shell but leaves $ unescaped, so any "$1"/"$VAR" in the command expands in that shell (empty) — never pass a $-bearing argument under -i.
- [`../agent-fabric/memory/shared/domain-claude-code-attribution-reminder.md`](../agent-fabric/memory/shared/domain-claude-code-attribution-reminder.md) — Where Claude Code's Co-Authored-By / Generated-with reminder comes from, what switches it off, and what a session that still sees one means (shared)

## solution

- [`.agent-fabric/memory/devex-tooling/solution.md`](.agent-fabric/memory/devex-tooling/solution.md) — How to measure a CI display/AT-SPI wrapper for real on develop-qzapp, which has no Xvfb or dbus-run-session

## workflow

- [`.agent-fabric/memory/devex-tooling/workflow/ci-command-line-is-read-by-guards.md`](.agent-fabric/memory/devex-tooling/workflow/ci-command-line-is-read-by-guards.md) — Before changing a ci.yml run: line in InterWeave, run the full cargo xtask checks — other guards grep the workflow's command lines
- [`.agent-fabric/memory/devex-tooling/workflow/review-class-cannot-write-scratch.md`](.agent-fabric/memory/devex-tooling/workflow/review-class-cannot-write-scratch.md) — A code-review dispatch cannot write any file, scratchpad included — it cannot build fixture trees or run a guard's self-test on a mutated copy

## recall

- [`../agent-fabric/identities/roles/devex-tooling/recall.md`](../agent-fabric/identities/roles/devex-tooling/recall.md) — Where devex-tooling's knowledge lives — charter, remit, distilled slices, this agent's memory — and how to trace a claim to its sources.
