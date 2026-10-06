---
role: "rust-ui-dev"
class: index
description: "What rust-ui-dev knows and where it lives."
tier: 1
distilled_at: "2026-10-05"
---

# rust-ui-dev — knowledge index

A session is given the charter and brief (in the launch prompt),
and the project's remit with a pointer to this index (from the
session-start hook) — nothing below. Open a slice when its cue
matches what you are doing; a `workflow` slice says how a kind of
work is done here, so read the matching ones before that work.
Paths are relative to this working copy; `../agent-fabric/` is the
control plane checked out beside it.

## charter

- [`../agent-fabric/identities/roles/rust-ui-dev/charter.md`](../agent-fabric/identities/roles/rust-ui-dev/charter.md) — The fleet's native-client developer in Rust: the user interface on Slint across desktop and mobile, its accessibility, the client's local store and its privacy posture, and the acceptance tests a person's use of it must pass.

## brief

- [`../agent-fabric/identities/roles/rust-ui-dev/brief.md`](../agent-fabric/identities/roles/rust-ui-dev/brief.md) — How rust-ui-dev works day to day, in any project: a change begins from the person's journey, is designed as well as built, rests on agreed contracts, and is verified in the rendered client on every platform before it is called done.

## domain

- [`../agent-fabric/memory/domains/rust-ui-dev/domain/atspi-e2e-lessons.md`](../agent-fabric/memory/domains/rust-ui-dev/domain/atspi-e2e-lessons.md) — Driving a Slint/AccessKit window over AT-SPI in tests -- what the adapter answers, focus under Xvfb, hearing announcements, SELinux on this host
- [`../agent-fabric/memory/domains/rust-ui-dev/domain/slint-rendered-window-lessons.md`](../agent-fabric/memory/domains/rust-ui-dev/domain/slint-rendered-window-lessons.md) — Slint 1.18 layout/font traps the testing backend does not show -- monospace by name, wrap min-width, a component's height from its only child layout; look at the winit window under Xvfb

## solution

- [`.agent-fabric/memory/rust-ui-dev/solution/stage-15-b2-pr178.md`](.agent-fabric/memory/rust-ui-dev/solution/stage-15-b2-pr178.md) — Stage 15 B2+B3 = PR #178 (branch develop-qzapp/rust-ui-dev-01/feat/stage-15-desktop): app start-up/running + store v7 read_pairs; review history and SQLite lessons
- [`.agent-fabric/memory/rust-ui-dev/solution/stage-15-client-reading-notes.md`](.agent-fabric/memory/rust-ui-dev/solution/stage-15-client-reading-notes.md) — What the remit's reading list (ADR-0039/0040, clients/human/*, plan §17-§18) means for the desktop client work from Stage 15 — the non-obvious points

## workflow

- [`.agent-fabric/memory/rust-ui-dev/workflow/arming-tool.md`](.agent-fabric/memory/rust-ui-dev/workflow/arming-tool.md) — How to arm a PR in InterWeave since 2026-10-04: tools/gh/arm.sh, not gh pr merge --auto by hand
- [`.agent-fabric/memory/rust-ui-dev/workflow/desktop-e2e-stale-binary.md`](.agent-fabric/memory/rust-ui-dev/workflow/desktop-e2e-stale-binary.md) — desktop-e2e runs whatever human-desktop binary sits in target/ -- rebuild it before a package-only test run or a mutation check

## threads

- [`.agent-fabric/memory/rust-ui-dev/threads/stage-15-b1-pr176.md`](.agent-fabric/memory/rust-ui-dev/threads/stage-15-b1-pr176.md) — Stage 15 B1 = PR #176 (branch develop-qzapp/rust-ui-dev-01/feat/stage-15-core): what it holds, its review history, and lessons for later batches
- [`.agent-fabric/memory/rust-ui-dev/threads/stage-15-b4-pr181.md`](.agent-fabric/memory/rust-ui-dev/threads/stage-15-b4-pr181.md) — Stage 15 B4 / PR #181 (merged 1c2f8dc2, 2026-10-04) -- desktop window, deny delta, what it left open for B5-B7
- [`.agent-fabric/memory/rust-ui-dev/threads/stage-15-b6-body.md`](.agent-fabric/memory/rust-ui-dev/threads/stage-15-b6-body.md) — Stage 15 B6+B7 (drawn body, one announcement, AT-SPI e2e) -- local branch state and what waits on #183
- [`.agent-fabric/memory/rust-ui-dev/threads/stage-15-rulings.md`](.agent-fabric/memory/rust-ui-dev/threads/stage-15-rulings.md) — architect-cto's Stage 15 rulings Q1-Q13 (relay seq 11163, 2026-10-03) — app-core, store path, recovery, read_pairs, re-keep, E2E evidence, inputs; check before building any Stage 15 batch
- [`.agent-fabric/memory/rust-ui-dev/threads/transport-client-facade-contract.md`](.agent-fabric/memory/rust-ui-dev/threads/transport-client-facade-contract.md) — The agreed caller-facing contract of crates/human/transport-client (Stage 14 batch 5), before its PR quotes it
- [`.agent-fabric/memory/rust-ui-dev/threads/ui-model-surface-proposal.md`](.agent-fabric/memory/rust-ui-dev/threads/ui-model-surface-proposal.md) — ui-model's surface (Stage 14 batch 6) as proposed by p2p-network-dev and amended by rust-ui-dev, before it is built; I own ui-model from Stage 15
- [`.agent-fabric/memory/rust-ui-dev/threads/ui-slint-stage15-carry.md`](.agent-fabric/memory/rust-ui-dev/threads/ui-slint-stage15-carry.md) — What I inherit in crates/human/ui-slint at Stage 15 from PR #170 — the root's drain contract and carried P3s; read before writing apps/human-desktop's root or first touching ui-slint
- [`.agent-fabric/memory/rust-ui-dev/threads/ui-slint-surface-proposal.md`](.agent-fabric/memory/rust-ui-dev/threads/ui-slint-surface-proposal.md) — ui-slint (Stage 14 batch 8) surface proposed by p2p-network-dev (seq 10823) and my amendments U1a-U5c (seq 10826); check they land in its PR — I own ui-slint from Stage 15

## recall

- [`../agent-fabric/identities/roles/rust-ui-dev/recall.md`](../agent-fabric/identities/roles/rust-ui-dev/recall.md) — Where rust-ui-dev's knowledge lives — charter, remit, distilled slices, this agent's memory — and how to trace a claim to its sources.
