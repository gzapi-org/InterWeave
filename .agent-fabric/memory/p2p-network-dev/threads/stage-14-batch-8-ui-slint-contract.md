---
role: "p2p-network-dev"
class: threads
topic: "stage-14-batch-8-ui-slint-contract"
description: "Stage 14 batch 8 (ui-slint) surface agreed with rust-ui-dev-01 (relay seqs 10823, 10826, 10849); slint graph measurements; replay for full text"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 8e68e8f39cb66fc6
---

## Stage 14 batch 8 (ui-slint) surface agreed with rust-ui-dev-01 (relay seqs 10823, 10826, 10849); slint graph measurements; replay for full text

Branch: develop-qzapp/p2p-network-dev-01/feat/stage-14-ui-slint, off ca74f6cf (after #169).

**Graph.**
- slint =1.18.1, default-features off, features std + compat-1-2. No windowing backend and no renderer.
- i-slint-backend-testing =1.18.1 (U1a: exact pin, since i-slint-* make no semver promise), dev-only, default-features off.
- Measured: 276 packages. Advisories, bans and sources pass. The only rejection is the Slint licence, on 7 crates: slint, slint-macros, i-slint-core, i-slint-core-macros, i-slint-common, i-slint-compiler, i-slint-backend-selector. It goes in as per-crate [[licenses.exceptions]] for LicenseRef-Slint-Royalty-free-2.0 (owner decision, plan §17, 2026-10-01).
- The desktop set (winit + femtovg + accessibility) measures 555 packages. It brings BSL-1.0 (arboard, clipboard-win), RUSTSEC-2026-0192 (ttf-parser unmaintained) and skia-bindings: Stage 15's.

**Agreed surface.**
- .slint files via slint-build. One AppWindow: conversation list, message list, composer, connectivity, session notice.
- View::render(&UiModel, selected) updates the Slint models BY KEY (U3a), never a whole-model replace. Test: focus kept on an item across a new message.
- on_intent sink. Actions come only from UiModel::actions(key), as buttons, plus "view source".
- conversation_viewed runs on a selection change AND on a focused-flag change (U3b). Test: unfocused gives no MarkRead, then focus gives MarkRead.
- U2a: a conversation row says direct or channel. U2b: retention state as text, including kept on an unread copy. U2c: Reply::Unavailable shows a placeholder.
- U2d = (b): the body is `source` as literal text, no link element (asserted), no OpenLink. Subset rendering is carried to Stage 15 by name (tell architect-cto).
- U4: one English table, marked "development placeholder copy, unreviewed". Templates, no concatenation. Ids inserted verbatim. No Read/Seen/Delivered text.
- U5a: the full PeerId goes in accessible-description on the author/route element and the conversation header only. An item's label is short author, then status, then body.
- U5b: tree test. The AcceptedV2 text is not Read/Seen. Unknown connectivity is not Offline. invoke_accessible_default_action gives the expected Intent. Tab reaches the composer, send, item actions and the notice action, or the limit is stated.
- Live region: Slint 1.18 HAS accessible-live-region, and the testing backend reads it. Set it on status and connectivity and assert it.
- Unverified (state in the PR): platform announcement, contrast, scaling, reduced motion, PeerId copy.
- No trust controls (the trust bullet is carried to Stage 15); assert absence.

Carried into this PR: the #169 N1 P3, narrowing the human_chat.rs probe comment.

**Progress (2026-10-03).** #170 opened at 8b14131b with 4 work commits.

The first blind review found nothing at P1, 4 P2s and 3 P3s. rust-ui-dev's owner review found 3 P2s and 1 P3. All were fixed in 12 commits (8b14131b..5f648a2a), each mutation-checked:
- action models reused per item;
- focus and a render's "viewed" are state: never refused, newest only, so the cap counts presses only (bound: cap + 2);
- wake runs after the borrow drops;
- dirty composer, and no draft write-back while an edit is pending;
- minimal-move update_by_key, counted by a unit test;
- tree sweep for "{";
- short_peer, DirectTitle and AppTitle in the ui-model table;
- the refused send is a polite live region.

Measured: cargo-deny skips dev-dependency LICENCES unless licenses.include-dev is set, but its advisories cover them. i-slint-backend-testing is therefore unexempted on purpose, and deny.toml says so.

The re-review of the fix range is dispatched. The owner arms, since there are fewer than 8 work commits.

**Landed:** #170 merged as 630d02d6 on 2026-10-03, armed on the owner's word at d700cedb (4 work, 20 fix). All my Stage 14 batches (2–8) are done.

Carried to Stage 15, for rust-ui-dev (told over GZCoord):
- 2 comment-only P3s: INPUT_CAP's doc does not state the bound's failure case, and take_events' doc has a 108-character line;
- the drain contract for the root;
- the silent-missing-fonts risk under fontconfig-dlopen (a startup check is owed);
- rust-ui-dev's own F4 (live regions on every item).

Left for architect-cto: the ledger audit and the close. The envelope flip is on the owner's word.

*References: licenses.exceptions*

*Observed 2026-10-03 (p2p-network-dev)*
