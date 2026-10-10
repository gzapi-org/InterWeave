---
role: "p2p-network-dev"
class: threads
topic: "pr228-judge-before-create"
description: "#228 MERGED 0e17a32e 2026-10-08 (j35-j40): judge-before-create, FIFO-safe opens, ADR-0028 #231 private-group predicate, umask-independent suites, 120 s keepalive bound; what is carried"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - ebc266d166daeeea
---

## #228 MERGED 0e17a32e 2026-10-08 (j35-j40): judge-before-create, FIFO-safe opens, ADR-0028 #231 private-group predicate, umask-independent suites, 120 s keepalive bound; what is carried

PR #228, branch develop-qzapp/p2p-network-dev-01/fix/judge-before-create, work commit 53b958e5 (1 work). From rust-ui-dev-01's observation 01a11bb1 (seq 24000); replied seq 24026 (taken).
- Class fix at one interface: `create_private_dir` (persist.rs) finds the nearest existing component, `resolve_guarded_dir_as` on it, then `create_each_as` makes each missing name 0700 under the RESOLVED base; AlreadyExists adopted only via `resolve_owned_private_dir_as`; `..` in the missing part refused before any mkdir. Reaches both locks and both private writers.
- Tests: persist.rs units (refused ancestor / appeared component / climb), human_lock.rs `a_refused_ancestor_of_a_missing_state_dir_creates_nothing`, persistence.rs `key_material_under_a_refused_ancestor_creates_no_directory`. Mutations M1-M3 all caught (M1 by all three ancestor tests, with --no-fail-fast).
- Owed: tell rust-ui-dev-01 when it lands so they add the creates-nothing assertion to apps/human-desktop/tests/startup.rs; human-store's own private_dir/create_each (#226) could call this — their call.
- Under 8 work commits: per [[carry-commits-count-as-work]] add work (pr224 carried risks: FIFO config.yaml with no O_NONBLOCK, umask-002 test writes) rather than ask.
- Round 1 (2026-10-08): blind review at 53b958e5 posted (P3-1 ownership half untested); codex P2 (existing non-dir => Ok) judged real P3. Fixes 25c2ea7f (AlreadyExists for non-dir), c40c706c (/tmp other-uid adopt test), d4a39973 (resolve_private_dir refuses non-dir); mutations caught. Codex thread answered+resolved. CodeQL 42 dismissed (standing policy). Local xtask ci at 53b958e5 green once ~/.local/share/cargo-tools/bin is on PATH (machete). Re-review of 53b958e5..d4a39973 dispatched. Main moved 19 commits (human-store/desktop/remit), no overlap; merge origin/main before arming.
- Round 2 re-review clean, posted at d4a39973. Then: merge origin/main b7623cde; 5f8e59b8 resolve_guarded_dir refuses a non-directory (create_private_dir under a file never reaches it: its walk meets ENOTDIR first — the re-review's claim otherwise was wrong); 343f44c3 j36 (O_NONBLOCK on config.yaml + overlay opens, FIFO tests with a 5 s timeout that releases the reader). Review of 5f8e59b8+343f44c3 dispatched.
- j37 BLOCKED: under umask 002, 56 tests fail at origin/main 36e6adc6 (tempfile dirs 0775 refused by the ancestor rule), #228 adds 5. Raised as an ADR-0028 question to architect-cto (seq 24742): does a UPG group-writable ancestor pass? Tests fixed only after the ruling.
- Round 3 review clean (5f8e59b8+343f44c3), posted. Key-file FIFO window (profile-identity plain File::open after symlink_metadata) left as a stated limit: untestable, same-uid only.
- j38 a539f040: private-group predicate (persist.rs owners_private_group + NameService/HostNames via nix 0.30.1 "user", linux-gated; load.rs file clause uses it). 5 mutations caught. Disk tests that staged group-write moved to other-write (host groups would decide them).
- j37 de4d22fc/9e43f82b/5579f2fd/17efb885/ba47a6bc: tests use tempfile::Builder::permissions(0700) + config.yaml 0644. Reproduce the hostile host with `sg otscache -c 'umask 002; cargo test ...'` (otscache = my shared supplementary group): 61 fail before, 0 after; desktop-e2e daemon control 19/19 fail with the fix reverted. Told rust-ui-dev (seq 25055) their human suites have the same shape.
- j39 queued (keepalive interval+timeout <= 120 s, from 02 seq 24854, replied 24863). Review of 343f44c3..ba47a6bc dispatched; xtask ci running at ba47a6bc.
- j39 note: the 120 s is interweave_ipc_protocol::CLIENT_SILENCE_TIMEOUT on 02's #230 (eb24bdfa, unmerged). j39 takes 120_000 from LOCAL-IPC.md now; a dev-dependency pin test (profile-config bound == CLIENT_SILENCE_TIMEOUT) is owed once #230 merges.
- The predicate's ADR amendment is architect-cto's #231 (c29c8376, 1 work, owner's word arms). Cite #231 in #228's body; arm #228 only after #231 merges (code must not land ahead of its decision). #220 merged cff230a5.
- Round 4 review (a539f040..ba47a6bc) posted: F1 P2 root:root accepted via owner-not-euid — answered by j40. j40 2bf9e3e2 (euid, gid in detail, access ACL via rustix lgetxattr/fgetxattr; 5 mutations caught after adding config.yaml ACL test), j39 241b7598 (120 s bound; 2 mutations caught). Merged origin/main 230a2976. PR body rewritten (all jobs, evidence, limits, "lands after #231"). Review of 2bf9e3e2+241b7598 dispatched; xtask ci at 230a2976 running. Arm when: review clean on head, CI concluded, #231 merged.
- Round 5 review (2bf9e3e2+241b7598) posted: 5 P3s (F1 Cargo comment "no crate added" false for daemon graph; F2 config.yaml euid-vs-owner untested → pure seam; F3 "ACL asked only of group-writable" untested; F4 detail wording + user-lookup failure named as group + io error dropped — keep ADR term "owner" (= daemon euid per #231), append cause; F5 ipc.rs "three rules" → four). Architecture texts + R1 NFSv4: architect-cto takes texts after merge, R1 a stated limit (seq 25702). Fixes wait for xtask ci3 to finish.
- Rounds 6-7: N1 (group-lookup refusal names no account) fixed 2c10ac25; round-7 P3-1 (Err arm untested: FakeNames.fails failed the user read first) fixed e31160ee with a group_fails flag; both mutation-checked. post-review.sh pins the CURRENT head only — round 7 (of 2c10ac25) is posted combined with round 8 (of e31160ee), never alone after a push.

- **MERGED 0e17a32e, 2026-10-08 19:14 UTC**, armed after #231 merged (owner's direct word "Arm #231, then #228"; relays of "merge 228" from rust-ui-dev and 02 did not set an order). Told rust-ui-dev (creates-nothing assertion can land), 02, architect-cto.
- Carried: pin test CLIENT_SILENCE_TIMEOUT_MS vs ipc-protocol's constant (job, blocked on #230); identity key FIFO window (stated limit); NSS hang with no timeout (risk); NFSv4 ACLs (ADR stated limit).
- Lessons: an Agent prompt is literal (a shell substitution went verbatim; see [[agent-prompt-does-no-shell-expansion]]); a python heredoc carrying prose must be quoted (<<'PY') or the shell runs what it quotes; a watcher must exit on failure too; CodeQL uid/user-name alerts recur on every push touching refusal tests — dismiss per devex policy.
- j41 → PR #236 (1 work, unarmed, review clean at 5e55e061; branch develop-qzapp/p2p-network-dev-01/test/client-silence-pin). Under the floor: next profile-config work joins it. Optional carried: compare Durations instead of as_millis (truncation).
- #236 batch (2026-10-09, owner said "on 236"): j42 190c1c4d (Duration pin), j43 09021beb (another_uid non-root precondition on the ancestor/link-rule tests only), F1 doc fix 7741538e, j44 6cb59136 (NSS reads under NSS_READ_DEADLINE 5 s on a helper thread; refuses "did not answer within 5s"; NameService: Clone+Send+'static). Wording question to architect (seq 29330): kept "owner's" vs their "daemon user's".
- #236 rounds 4-6: F1 (spawn/disconnect/timeout named apart) + F3 (names_clone gone) + control 2 s → c9107076; N1 doc + panic bound <4 s → c6e39653; clean at c6e39653. #236 = 4 work, 3 fix: arms after architect's ADR "The name-service read is bounded" PR merges (not opened yet at 2026-10-09 02:30) — and needs the owner's word or more work (under 8).
- j45 f8b8aa33 (NSS_READ_OUTSTANDING: one read per process; refuses "an earlier name-service read has not returned"; Drop clears on return/panic; per-test guard). architect-cto: conforming to ADR-0028 text in #239 (3 work 2 fix, docs). #236 = 5 work, 3 fix; arms after #239 merges AND (8 work or owner's word).
- #236 later (2026-10-09): d44111e4 (guard via NameService::outstanding(), required; F1 parallel tests 257/300 → 0), 1693dd82 j46 (ReadGate Mutex+Condvar, waiters wait up to own deadline; architect ruling (b) seq 29607), 1abd4b48 (pin HostNames → process gate). Round 8 (of d44111e4) saved, to post combined with the review of 1693dd82..1abd4b48. A reviewer saw the wait test fail once at 1693dd82 (mutant-shared target suspected); I got 0/50 + 0/20. #239 = architect's ADR text, awaiting owner's word.
- #236 head 0e0b9484 review-clean (round 10 posted): a96330d0 (back-to-back asserts gate free per answer; reorder mutation 5/5), 0e0b9484 (one-budget test; recv_timeout(deadline) mutation fails; "flag" → "gate"). 6 work, 7 fix. Arms after #239 (4 work 5 fix, green, reviewed, waiting owner's word) merges, and needs owner's word (under 8). Reviewer leftovers cleaned each round (~650 MB each).
- **#236 MERGED 2405b7ce (2026-10-09)** after #239 (architect's ADR text) on the owner's "merge both". Carried: R2 (a read entering with no budget left still spawns its thread; an early `left.is_zero()` refusal would skip it); the optional per-walk (euid, gid) answer reuse architect permitted; R1 (a hung service blocks each trust write's task 5 s). Target cleaned 1.8 GiB.
- 2026-10-09 architect-cto's plan (seq 32615): (1) j47 → PR #241 (zero-budget read starts no thread; review-clean at 81dd6f7f, 1 work 3 fix, unarmed — needs owner's word or more work); (2) j48 §20 step 1: embedded runtime host on Android with the in-process adapter + trust boundary plumbed; NOT gated by SPIKE-008/009; tell architect the crate/seam before the first commit, and agree the Service seam with rust-ui-dev by REQUEST; (3) j49 §20 step 5 runtime side, the network-change binding API. architect closes the spikes. j48/j49 topic stage-17-runtime → fresh session.

*References: agent-prompt-does-no-shell-expansion, carry-commits-count-as-work*

*Observed 2026-10-09 (p2p-network-dev)*
