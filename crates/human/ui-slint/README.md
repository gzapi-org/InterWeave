# ui-slint

The human client's reference views (plan §17 (8)): Slint components bound to `ui-model`, shared by the desktop and Android apps when they arrive (Stage 15, Stage 17). A view reads a `UiModel` and nothing else. A person's actions leave it as `ViewEvent`s, so it never calls the facade, the store or IPC. Among `crates/human/*` it is the only crate whose graph names `slint` (P2).

**Current status:** active workspace member since Stage 14 batch 8. It is tested through Slint's testing backend: the accessibility tree, default actions and keyboard focus, with no window system. Since Stage 15 batch 4 the desktop app shows it in a real window.

## The contract

The surface was agreed with the client's role, which owns this crate from Stage 15, before it was built (relay seqs 10823, 10826, 10849, 10867, 10870, 10879, 10882, 10885).

**Graph.** By default the crate takes `slint` 1.18.1 with no windowing backend and no renderer: `std` and `compat-1-2` only. The `desktop` feature adds the window the owner chose on 2026-10-04 (plan §18 (14)):
- winit, with the software renderer and the platform accessibility adapter;
- `unstable-winit-030`, the only public way to read the window's focus.

The desktop app turns `desktop` on. `i-slint-backend-testing` is pinned to the same exact version and used for tests only. The pins are exact because the `i-slint-*` crates make no semver promise. `deny.toml` admits the Royalty-free licence for the Slint crates alone, by the owner's decisions of 2026-10-01 and 2026-10-04. Its attribution is the badge on the download page at Stage 19, not a screen (architect-cto's ruling).

**Event loop (desktop).** The app cannot name Slint, so the loop's pieces are here:
- `View::handle()` runs the loop;
- `invoke_on_window` hands a turn over from another thread;
- `defer` schedules a turn after the current event;
- `quit_event_loop` ends the loop;
- `View::on_window_focus` reports winit's focus, which drives `set_window_focused`.

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
- A focus change is state too. It is never refused, and it replaces a queued focus change only when that is the last input. With edits and focus changes, the queue holds at most four times the cap plus three (`queued_inputs`), provided the root takes until a take returns nothing before it returns to its event loop. A root that stops between takes can let one more edit wait per conversation selected meanwhile: still finite, but not that number.
- A new edit replaces a queued one only while nothing later for that conversation follows it.
- A render never writes the draft over an edit that is still queued.
- `set_wake` lets the root run a take as soon as a window callback queues input. The hook runs with none of the view's state borrowed, so it may take at once. The root's own calls (`select`, `set_window_focused`, `render`) do not run it.

**Text.** Every string comes from `ui-model`'s placeholder table (`placeholder_en`, `UiText`, `fill`). That copy is unreviewed development text (architect-cto's ruling, relay message 01a0fe85-6b39-7d6e-8b2a-f0cc4280c7a8). Templates are filled by name, and identifiers are inserted verbatim.

**Bodies.** A message body is drawn from `chat-protocol`'s block tree, the one parse of the remote bytes, flattened by `body.rs` into lines of plain text: headings, code, quotes, list items with their markers, table rows and rules. Inline marks are drawn as their text alone, and no remote-derived text reaches a second parser such as `StyledText`. A link's label stays in place, and its allowlisted destination becomes a separate control labelled with the full destination, with every control character and every character Unicode makes default-ignorable shown as its code point (`<U+202E>`), so no directional control reorders the label. Right-to-left letters in a destination are laid out by the ordinary bidirectional rules, as in any text. The view raises `OpenLink` only when a person activates that control, and `ui-model` checks the scheme again first. An image is a placeholder naming its alt text and is never fetched. Each message offers Show source and back: the source is shown in place of the drawn body, its link controls included, and a body past a bound is its source as plain text with no link controls either.

**Accessibility.**
- Every message item, route, connectivity indicator, composer, send control and notice action has a role and a label.
- An item's label is the short author, then the status, then the body. The full `PeerId` is on the author and in the conversation header, in exact form, and never in every item's label.
- Connectivity, the notice and a refused send are polite live regions, each one element. An item's status is not: the window has one announcement, a polite live region, that says what changed since the last render. That covers a message arriving in the open conversation (who sent it, or how many arrived, never the text), a status change on the person's own messages there, and arrivals in other conversations (by title, or how many conversations). Opening a conversation announces nothing. The announcement uses two slots that take turns, so the same sentence twice is still a change.
- Every control has a default action, and Tab reaches each one.

**Platform check.** `platform_check()` tells the app, before any window, whether fontconfig loaded. The toolkit opens it at run time, and a host without it runs with no fonts, silently. It asks the font stack's own loader (`yeslogic-fontconfig-sys`'s dlopen result), so its answer and the fonts cannot disagree.

## What it does not prove

The tests read Slint's own accessibility tree. They do not prove:
- that `platform_check` fails on a host without fontconfig: the loader searches the system's library paths, which a test cannot hide;
- that the platform adapter (in the graph since batch 4) exports the tree as these tests read it: the AT-SPI cases are batch 7's;
- that a live region is announced: the tests read what the regions hold, and whether a screen reader speaks it, and speaks a slot that goes from empty to the same sentence, is the platform adapter's (batch 7's AT-SPI cases, and a person with a screen reader);
- layout under the platform's fonts. The testing backend's layout differs from the winit window's: a link control that kept one line's height in the window was sized correctly there. The body was inspected in the rendered window under Xvfb: lists, quotes, monospaced code, tables, a long unbroken word, a long destination. A test holds only the window width;
- text scaling or reduced motion. The colours are chosen for WCAG AA contrast, but no tool has measured them on the rendered window;
- copying a `PeerId` through the clipboard. The open conversation's identifier is a read-only, selectable field whose value the tree holds exactly (tested); the copy itself is the platform's;
- a visible scrollbar: the conversation and message lists scroll by wheel and touch, and the toolkit brings a row that takes keyboard focus into view (tested); that a long conversation list leaves the window's minimum height alone is not tested -- the testing backend keeps the window's size.

Stage 14 has no trust controls. The trust bullet of `human-client-ui.md` §13 is carried to Stage 15, and the tests assert that no such control exists.

The code `slint-build` generates allows `unsafe_code` locally, so this crate has no `forbid(unsafe_code)`. The workspace's `deny` still covers every handwritten line.
