---
role: "p2p-network-dev"
class: threads
topic: "pr224-carried-hardening"
description: "PR #224 (2026-10-08): ancestor rule (ADR-0028 A 2026-10-08) + #190/#192/#199 carries + lockfile refresh; state and what is still open"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - a6b820f6f3329672
---

## PR #224 (2026-10-08): ancestor rule (ADR-0028 A 2026-10-08) + #190/#192/#199 carries + lockfile refresh; state and what is still open

PR #224, branch develop-qzapp/p2p-network-dev-01/fix/carried-hardening, opened 2026-10-08 at 96ed137b (9 work by pr-gate). Jobs j30-j34.
- Ancestor rule: architect-cto's ruling seq 20326 + revisions 20431 (funnel is require_private_dir; links judged, open the resolved path; sticky still root's/ours) + 20512 (config.yaml dir judged for writers). Supply 9edf74d0 folded at e23e51c9. Code ca1ef42b (profile-config: resolve_private_dir_as / resolve_judged_as / judge_ancestor / judge_link / resolve_guarded_dir; writers, lock, ProfileConfig::load under the resolved path; LoadError::ConfigDirUnguarded) and 96ed137b (identity loader).
- Untestable, stated in the PR: the open-under-resolved-path race; off Linux the private writers now refuse too.
- Two blind reviews dispatched (one per area: ancestor rule; ipc-client/conformance/lockfile). CI rerun on 96ed137b.
- Lesson: tempfile::tempdir's mode follows the umask; with_display.sh's run gave 0755, so set modes explicitly in tests (4c21f63e).
- Lesson: the j31 and #190 outbox carries were already done on main (53000a4f, 8fd48ed6); read origin/main before taking a carry from memory.

**Round 2 (2026-10-08, resumed after a session restart that killed the background CI):** both reviews posted at 96ed137b (A: P2 owner-test vacuous, P3s; B: P2 writer end code overwrote the server's close). Fixed: 6eb4bb58 (EndedBy::Writer provisional, ServerClose replaces), ae3d3d10+76744e95 (owner tests under /tmp), the F3/F2/F4 comment commits, 9cfe9258 (overlay read resolved), 3ff90cc1 (config.yaml O_NOFOLLOW + owner/write bits, LoadError::ConfigFileUnguarded); architect supply 738812eb folded at da8298a9; origin/main merged at e4a4dc0a. Two CodeQL "cleartext uid" threads answered as not findings and resolved. Re-review of 96ed137b..3ff90cc1 dispatched; xtask ci running. Gate: 12 work, 6 fix. Owner said "merge": arm once the re-review is clean and checks conclude.
Lesson: the scratchpad is wiped on restart; rebuild previous findings from the posted reviews (gh api .../reviews).
Risk to state: tests that write config.yaml with fs::write would be refused under umask 002 (CI and this host are 022).

**Armed 2026-10-08** at cecd2fd9 (12 work, 7 fix), queued at 2. Round 3 re-review clean after N1 (config.yaml owner clause test via open_guarded_as). CodeQL went red on rust/cleartext-logging (a uid in refusal text the ADR requires, and test panics printing errors): owner chose "dismiss as false positive, then arm"; alerts 22-41 dismissed with the reason. CodeQL is not a required check (#210 merged red too).
Carried risks: a call in flight when the writer fails answers BackendUnavailable, not the server's close code (session end is corrected); FIFO named config.yaml blocks start (no O_NONBLOCK); owner tests assume non-root; tests writing config.yaml with fs::write break under umask 002; encode-failure PayloadTooLarge end untested; events(MAX)==events(0) only with nothing buffered (contract question).
**Merged** cfd490ab 2026-10-08.

*Observed 2026-10-08 (p2p-network-dev)*
