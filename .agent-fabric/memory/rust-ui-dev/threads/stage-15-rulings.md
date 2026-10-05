---
role: "rust-ui-dev"
class: threads
topic: "stage-15-rulings"
description: "architect-cto's Stage 15 rulings Q1-Q13 (relay seq 11163, 2026-10-03) — app-core, store path, recovery, read_pairs, re-keep, E2E evidence, inputs; check before building any Stage 15 batch"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-05"
origin:
  - agent: "rust-ui-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - d587a2ff10140431
---

## architect-cto's Stage 15 rulings Q1-Q13 (relay seq 11163, 2026-10-03) — app-core, store path, recovery, read_pairs, re-keep, E2E evidence, inputs; check before building any Stage 15 batch

Stage 14 closed by #171 (marker `stage-15-desktop-human-client`, main 4489ee24). architect-cto ruled my Q1-Q13 (seq 11163); §18 rewritten in §16's shape is their next PR after #174 — once merged, the plan text is the record, not this.
- Q1 new `crates/human/app-core`: headless root (Command/Update, facade side + model side), no tokio/Slint/platform/rusqlite (store via transport-client). ViewEvent → ui-model. I add planned_members + landing-zone README in my first batch; architect-cto does layout doc; devex grows layering guard.
- Q3 `<profile XDG state>/human/human.sqlite` + single-instance lock beside it; derivation is ProfilePaths' → ask p2p-network-dev for `human_dir()`.
- Q4 Stage 15 recovery = blocking screen, no session/lease, path shown, Quit/Retry; app never renames/moves/deletes the file. Read-only/export carried.
- Q5 `read_pairs(origin, app_message_id, at)` only, verify_shape-pinned, 4096 rows, oldest evicted in the insert txn, written by mark_read AND unkeep, same-id different-text re-send suppressed. Joins owner's privacy review.
- Q6 re-keep allowed within session: unkeep returns the session copy (ReadEphemeral); new store-half case beside 8/10 (no renumbering of RETENTION §9).
- Q7 a11y on real adapter: AT-SPI in CI via devex; a release-test record counts only if §18's gate names it as a limit. Seeding pending_outbound = SEND half of shipped proof; RECEIVE half must be observed from the receiving binary (tree or inspection surface), both halves in one run.
- Q8 --profile required; endpoint/channels from profile config, never UI. Q9 guidance only, autostart carried (owner/packaging). Q10 tests/desktop-e2e/tests/human_app/ mine; Cargo.toml shared, name other lane in PR body. Q11 no new-conversation-by-PeerId. Q12 non-unix stub. Q13 `DataSessionPort::ready()` approved; p2p builds with server_state.
- Q2 backend: I send measured evidence; architect-cto brings one recommendation to the owner.

Related: [[ui-slint-stage15-carry]], [[stage-15-client-reading-notes]]

**Q2 (seq 11172 evidence, 11178 ruling):** winit + renderer-software + accessibility recommended to the owner (smallest Linux graph 460 pkgs, no GL/C++, AT-SPI via accesskit_unix). BLOCKER for any winit renderer: i-slint-backend-winit 1.18.1 has a non-optional cfg(iOS) dep on renderer-skia → glow 0.18 → js-sys ~0.3.100, vs libp2p-swarm 0.48.0's cfg(wasm) `wasm-bindgen-futures =0.4.58` → js-sys =0.3.85 (lockfile is target-agnostic). Way out is p2p-network-dev's: vendor libp2p-swarm (a) or wasm-bindgen-futures 0.4.58 (a') under third_party; second workspace refused. B1-B3 build against the pin as is; B4 first needs it. Deny delta (owner's): Slint RF exceptions for new i-slint crates; RUSTSEC-2026-0192 ignored with reason + revisit at next Slint bump; BSL-1.0 NOT admitted — cargo-deny graph restricted to Linux targets until Windows.
**Signing:** this login lacks the fleet's shared commit key (82AE…); owner must hand it over in a terminal (fabric-coordinator seq 11169). Don't override or sign with own key.
**R4 human_dir (seqs 11187, 11202):** p2p contributor branch develop-qzapp/p2p-network-dev-01/for/rust-ui-dev-01/human-dir (5 commits off 4489ee24 incl. key_file refusal 960cd41e, f33c82c9; Supplier-Review) — fold unrebased into B2; app must call roles_are_distinct() at start and refuse on false; asked p2p to also refuse identity.key_file inside human/ (seq after 11187).
**Copy seam (InterWeave#173, seq 11231):** I own the keys in ui-model labels.rs; a key I add ships with an English placeholder I draft; language-culture finalises and delivers by locator; I commit with the Supplier-Review trailer they name. Until English has a holder, architect-cto reads each placeholder against human-client-ui.md §5/§12 — a PR adding one asks for that.
**Owner on licence delta (seq 11464, 2026-10-04, plan §18 (14) on #175 997dd707):** new i-slint crates admitted under Slint Royalty-free 2.0 per crate (same exception shape as the existing eight); BSL-1.0 NOT admitted — deny graph restricted to Linux targets. STILL PENDING before B4: renderer choice (renderer-software recommended) and the RUSTSEC-2026-0192 ignore. deny.toml edits are my B4 PR, after p2p ADR-0054 vendoring PR.
**Backend DECIDED (seq 11473, plan §18 (14) on #175 87a193f7):** winit + renderer-software + accessibility. B4 deny.toml edits: exceptions for new i-slint crates (Slint RF 2.0 per crate); RUSTSEC-2026-0192 ignored with reason (ttf-parser unmaintained, only via Slint font stack, revisit at next Slint bump); [graph] targets = Linux targets built. Order: #175 merges → p2p ADR-0054 vendoring PR (third_party/wasm-bindgen-futures) → my B4. Measure software-rendering CPU cost at B4.
**Attribution (seq 11740):** option (b) — Slint badge on the public download page at Stage 19 packaging; B4 has NO About/attribution surface. Fallback: if no public download page when first binary ships, in-app AboutSlint returns with its image-feature delta measured first. Recorded on architect-cto branch for/rust-ui-dev-01/p1-rusqlite-wording (fold into B4 with devex 2365ae37 stage15-supply).

*References: stage-15-client-reading-notes, ui-slint-stage15-carry*

*Observed 2026-10-04 (rust-ui-dev)*
