# ui-slint

The human client's reference views (plan §17 (8)): Slint components bound to `ui-model`, shared by the desktop and Android apps when they arrive (Stage 15, Stage 17). A view reads a `UiModel` and nothing else. A person's actions leave it as `ViewEvent`s, so it never calls the facade, the store or IPC. Among `crates/human/*` it is the only crate whose graph names `slint` (P2).

**Current status:** active workspace member since Stage 14 batch 8. It is tested through Slint's testing backend: the accessibility tree, default actions and keyboard focus, with no window system.

## The contract

The surface was agreed with the client's role, which owns this crate from Stage 15, before it was built (relay seqs 10823, 10826, 10849, 10867, 10870, 10879, 10882, 10885).

**Graph.** The crate takes `slint` 1.18.1 with no windowing backend and no renderer: `std` and `compat-1-2` only. `i-slint-backend-testing` is pinned to the same exact version and used for tests only. The pins are exact because the `i-slint-*` crates make no semver promise. Which backend ships is the apps' decision at Stage 15, together with the Slint Royalty-free licence's attribution duty. `deny.toml` admits that licence for the Slint crates alone, by the owner's decision of 2026-10-01.

**Rendering.** `View::render(&UiModel)` updates the lists by key: a changed row is replaced in place, and rows are inserted and removed where they changed. It never replaces a whole model, because that moves keyboard focus and a screen reader's place back to the start.

**Input.** A Slint callback cannot see the model, so it decides nothing. It queues what the person pressed, by identity: the item and the action as rendered, or the notice that was shown. `View::take_events(&UiModel)` then resolves each press against the model it is given:
- An action is emitted only if `actions(key)` still offers that same action. A press whose action has gone yields nothing, never a different action.
- A notice action is emitted only while its notice is still shown.
- A send goes through `send_draft`.
- Selection and window focus go through `conversation_viewed(key, focused)`, so a conversation is read only when it is shown while the window has focus.
- A render that shows a conversation with unread items marks the view "viewed". A message that arrives while the person reads it is therefore read at the end of the next take, if the window then has focus and the conversation is still shown. `render` does not wake the root, which is mid-render: the root takes after it renders.
- A take stops at the first draft edit. The root applies it and takes again, so a send queued after an edit reads the edited text.

The queue's order is the person's order: no queued input is ever moved relative to another, and nothing a person did is held outside it. Coalescing happens only in place. The one thing outside the queue is a render's "viewed", which is resolved once the queue has drained.

The queue holds `INPUT_CAP` presses:
- When it is full, the newest press is refused and counted. An edit is never refused: it is the composer's state, and refusing it would lose what the person typed.
- A focus change is state too. It is never refused, and it replaces a queued focus change only when that is the last input. With edits and focus changes, the queue holds at most four times the cap plus three (`queued_inputs`).
- A new edit replaces a queued one only while nothing later for that conversation follows it.
- A render never writes the draft over an edit that is still queued.
- `set_wake` lets the root run a take as soon as a window callback queues input. The hook runs with none of the view's state borrowed, so it may take at once. The root's own calls (`select`, `set_window_focused`, `render`) do not run it.

**Text.** Every string comes from `ui-model`'s placeholder table (`placeholder_en`, `UiText`, `fill`). That copy is unreviewed development text (architect-cto's ruling, relay message 01a0fe85-6b39-7d6e-8b2a-f0cc4280c7a8). Templates are filled by name, and identifiers are inserted verbatim.

**Bodies.** A message body is its source, shown as literal text. The markdown subset is not drawn in Stage 14, and no link exists to activate, so no `OpenLink` can come out of the view. Drawing the subset, with activation-only links, is carried to Stage 15.

**Accessibility.**
- Every message item, route, connectivity indicator, composer, send control and notice action has a role and a label.
- An item's label is the short author, then the status, then the body. The full `PeerId` is on the author and in the conversation header, in exact form, and never in every item's label.
- Status, connectivity and a refused send are polite live regions.
- Every control has a default action, and Tab reaches each one.

## What it does not prove

The tests read Slint's own accessibility tree. They do not prove:
- that a platform adapter exports the tree: no AccessKit adapter is in the graph until a backend is chosen;
- that a live region is announced. With every item's status a live region, an adapter that announces on insertion may read a whole list's statuses in a row. The client's role carries that to Stage 15, likely as one summary region;
- contrast, text scaling or reduced motion, since there is no renderer;
- copying a `PeerId`, since there is no clipboard without a backend.

Stage 14 has no trust controls. The trust bullet of `human-client-ui.md` §13 is carried to Stage 15, and the tests assert that no such control exists.

The code `slint-build` generates allows `unsafe_code` locally, so this crate has no `forbid(unsafe_code)`. The workspace's `deny` still covers every handwritten line.
