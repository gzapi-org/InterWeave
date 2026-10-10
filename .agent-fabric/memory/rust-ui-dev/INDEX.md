---
role: "rust-ui-dev"
class: index
description: "What rust-ui-dev knows and where it lives."
tier: 1
distilled_at: "2026-10-10"
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

- [`.agent-fabric/memory/rust-ui-dev/solution/android-network-change-contract.md`](.agent-fabric/memory/rust-ui-dev/solution/android-network-change-contract.md) — §20 step 5 (j44): what the Android Service's network callback owes EmbeddedHost::network_changed, and the wildcard-listener rule
- [`.agent-fabric/memory/rust-ui-dev/solution/android-spike-device.md`](.agent-fabric/memory/rust-ui-dev/solution/android-spike-device.md) — SPIKE-008/009 test device (Samsung A40, adb over network) and its baseline facts; toolchain provisioning is devex-tooling's
- [`.agent-fabric/memory/rust-ui-dev/solution/desktop-e2e-load-flakes.md`](.agent-fabric/memory/rust-ui-dev/solution/desktop-e2e-load-flakes.md) — j15 -- how to reproduce desktop-e2e timeouts under load, and the send-without-step defect found (Peer::deliver)
- [`.agent-fabric/memory/rust-ui-dev/solution/embedded-host-seam.md`](.agent-fabric/memory/rust-ui-dev/solution/embedded-host-seam.md) — the agreed seam between p2p-network-dev's embedded runtime (§20 step 1) and my Android Service, the human store and the audit sink
- [`.agent-fabric/memory/rust-ui-dev/solution/stage-15-b2-pr178.md`](.agent-fabric/memory/rust-ui-dev/solution/stage-15-b2-pr178.md) — Stage 15 B2+B3 = PR #178 (branch develop-qzapp/rust-ui-dev-01/feat/stage-15-desktop): app start-up/running + store v7 read_pairs; review history and SQLite lessons
- [`.agent-fabric/memory/rust-ui-dev/solution/stage-15-client-reading-notes.md`](.agent-fabric/memory/rust-ui-dev/solution/stage-15-client-reading-notes.md) — What the remit's reading list (ADR-0039/0040, clients/human/*, plan §17-§18) means for the desktop client work from Stage 15 — the non-obvious points
- [`.agent-fabric/memory/rust-ui-dev/solution/stage17-spikes-merged.md`](.agent-fabric/memory/rust-ui-dev/solution/stage17-spikes-merged.md) — SPIKE-008/009 device halves merged (#210, #214) and the human store's RETENTION 8 fix -- what landed and what each did not establish
- [`.agent-fabric/memory/rust-ui-dev/solution/ui-slint-body-cache-tests.md`](.agent-fabric/memory/rust-ui-dev/solution/ui-slint-body-cache-tests.md) — A views.rs test that loops over sources with one View must give each a distinct row — the view caches drawn bodies by ItemKey

## workflow

- [`.agent-fabric/memory/rust-ui-dev/workflow/arming-tool.md`](.agent-fabric/memory/rust-ui-dev/workflow/arming-tool.md) — How to arm a PR in InterWeave since 2026-10-04: tools/gh/arm.sh, not gh pr merge --auto by hand
- [`.agent-fabric/memory/rust-ui-dev/workflow/desktop-e2e-stale-binary.md`](.agent-fabric/memory/rust-ui-dev/workflow/desktop-e2e-stale-binary.md) — desktop-e2e runs whatever human-desktop binary sits in target/ -- rebuild it before a package-only test run or a mutation check
- [`.agent-fabric/memory/rust-ui-dev/workflow/labels-english-supplier.md`](.agent-fabric/memory/rust-ui-dev/workflow/labels-english-supplier.md) — who finalises English for crates/human/ui-model/src/labels.rs values, how it reaches my PR, and the conventions a new placeholder should follow
- [`.agent-fabric/memory/rust-ui-dev/workflow/prose-sweep-siblings.md`](.agent-fabric/memory/rust-ui-dev/workflow/prose-sweep-siblings.md) — before committing a human-client behaviour or copy change, sweep the sibling crate READMEs, .slint comments, the parity golden, and every test that asserts the old event behaviour (apps/human-* included)
- [`.agent-fabric/memory/rust-ui-dev/workflow/release-phone-after-testing.md`](.agent-fabric/memory/rust-ui-dev/workflow/release-phone-after-testing.md) — when device testing ends, run adb disconnect and adb kill-server so the shared Android phone is free
- [`.agent-fabric/memory/rust-ui-dev/workflow/render-brief-before-dispatch.md`](.agent-fabric/memory/rust-ui-dev/workflow/render-brief-before-dispatch.md) — fabric-review brief must render successfully BEFORE a review is dispatched; never dispatch in the same parallel batch as the render
- [`.agent-fabric/memory/rust-ui-dev/workflow/shared-target-worktree.md`](.agent-fabric/memory/rust-ui-dev/workflow/shared-target-worktree.md) — Never point a scratch worktree's build at the main checkout's target/ — binaries keep the worktree's CARGO_MANIFEST_DIR after it is deleted
- [`.agent-fabric/memory/rust-ui-dev/workflow/spike-harness-from-the-requirement.md`](.agent-fabric/memory/rust-ui-dev/workflow/spike-harness-from-the-requirement.md) — Before building a spike harness, re-read the governing ADR/SPIKES.md/design doc wording for each row — not my own plan's paraphrase

## threads

- [`.agent-fabric/memory/rust-ui-dev/threads/stage-15-b1-pr176.md`](.agent-fabric/memory/rust-ui-dev/threads/stage-15-b1-pr176.md) — Stage 15 B1 = PR #176 (branch develop-qzapp/rust-ui-dev-01/feat/stage-15-core): what it holds, its review history, and lessons for later batches
- [`.agent-fabric/memory/rust-ui-dev/threads/stage-15-b4-pr181.md`](.agent-fabric/memory/rust-ui-dev/threads/stage-15-b4-pr181.md) — Stage 15 B4 / PR #181 (merged 1c2f8dc2, 2026-10-04) -- desktop window, deny delta, what it left open for B5-B7
- [`.agent-fabric/memory/rust-ui-dev/threads/stage-15-b6-body.md`](.agent-fabric/memory/rust-ui-dev/threads/stage-15-b6-body.md) — Stage 15 B6+B7 (drawn body, one announcement, AT-SPI e2e) -- local branch state and what waits on #183
- [`.agent-fabric/memory/rust-ui-dev/threads/stage-15-b9-trust.md`](.agent-fabric/memory/rust-ui-dev/threads/stage-15-b9-trust.md) — Stage 15 B9 trust settings + gate (b) + B8 carry -- branch feat/stage-15-trust, what landed where, evidence levels, open asks
- [`.agent-fabric/memory/rust-ui-dev/threads/stage-15-rulings.md`](.agent-fabric/memory/rust-ui-dev/threads/stage-15-rulings.md) — architect-cto's Stage 15 rulings Q1-Q13 (relay seq 11163, 2026-10-03) — app-core, store path, recovery, read_pairs, re-keep, E2E evidence, inputs; check before building any Stage 15 batch
- [`.agent-fabric/memory/rust-ui-dev/threads/store-ancestor-rule-pr226.md`](.agent-fabric/memory/rust-ui-dev/threads/store-ancestor-rule-pr226.md) — j32 — human store routed through profile-config's ancestor walk (PR #226); Android /data 0771 system conflict raised to architect-cto
- [`.agent-fabric/memory/rust-ui-dev/threads/transport-client-facade-contract.md`](.agent-fabric/memory/rust-ui-dev/threads/transport-client-facade-contract.md) — The agreed caller-facing contract of crates/human/transport-client (Stage 14 batch 5), before its PR quotes it
- [`.agent-fabric/memory/rust-ui-dev/threads/ui-model-surface-proposal.md`](.agent-fabric/memory/rust-ui-dev/threads/ui-model-surface-proposal.md) — ui-model's surface (Stage 14 batch 6) as proposed by p2p-network-dev and amended by rust-ui-dev, before it is built; I own ui-model from Stage 15
- [`.agent-fabric/memory/rust-ui-dev/threads/ui-slint-stage15-carry.md`](.agent-fabric/memory/rust-ui-dev/threads/ui-slint-stage15-carry.md) — What I inherit in crates/human/ui-slint at Stage 15 from PR #170 — the root's drain contract and carried P3s; read before writing apps/human-desktop's root or first touching ui-slint
- [`.agent-fabric/memory/rust-ui-dev/threads/ui-slint-surface-proposal.md`](.agent-fabric/memory/rust-ui-dev/threads/ui-slint-surface-proposal.md) — ui-slint (Stage 14 batch 8) surface proposed by p2p-network-dev (seq 10823) and my amendments U1a-U5c (seq 10826); check they land in its PR — I own ui-slint from Stage 15

## recall

- [`../agent-fabric/identities/roles/rust-ui-dev/recall.md`](../agent-fabric/identities/roles/rust-ui-dev/recall.md) — Where rust-ui-dev's knowledge lives — charter, remit, distilled slices, this agent's memory — and how to trace a claim to its sources.
