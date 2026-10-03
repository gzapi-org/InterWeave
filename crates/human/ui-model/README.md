# ui-model

The human client's presentation state (plan §17 (6)). It sits between the transport facade (`crates/human/transport-client`) and the views. It is pure and synchronous, with no facade, no store and no I/O inside:
- the composition root feeds it what the facade and the store did;
- a view renders what it returns;
- a person's actions come back as `Intent`s, so a view never reaches the facade, the store or IPC.

It names the client's vocabulary (`interweave-human-client-api`) and nothing that reaches a store, a transport or a toolkit (plan §17 P2: no `rusqlite`, `libp2p` or `slint`).

**Current status:** active workspace member since Stage 14 batch 6, tested against `tests/local-client-fake` through the facade.

The person-facing English in `labels.rs` (`placeholder_en`, `UiText`, `fill`) is development placeholder copy, unreviewed, held in one module so Stage 15 can replace it whole (architect-cto's ruling, relay message 01a0fe85-6b39-7d6e-8b2a-f0cc4280c7a8). Its tests hold only what is structural: no delivery label reads as read, seen, processed or delivered; `Unknown` never reads as offline; every template is filled by name.

## The contract

Agreed with the client's role before it was built (relay seqs 10630, 10639 and 10642), and amended after #168's review (10707, 10710, 10713).

**Inputs:**
- the facade's `ClientEvent`s;
- `received` (from `drain`);
- `sent` (after the facade committed a row);
- `pending_listed`, `unread_listed` and `kept_listed` (from the store, at start and on `UnreadInStore`, merged by row id);
- the store acts' results: `read`, `kept`, `unkept`, and `copy_gone` when the root no longer holds the content a Keep would keep (Keep is then no longer offered);
- `send_pressed(key)` when the root issues a send, then `sent` or `send_refused`: an answer touches the composer only if it was not edited after that press, so text typed since, the same text typed again or a cleared composer included, is never overwritten (a refusal still shows why);
- `draft_changed`;
- the facade's diagnostics.

**Outputs:**
- `conversations()`: a direct route's title is its authenticated short `PeerId`, with the route label the peer asserts shown as a label beside it. The envelope's `from_endpoint` and the text never enter it.
- `messages(key)`, ordered by LOCAL time. The peer's `sent_at_ms` is display only. Each item has:
  - a stable `ItemKey`;
  - the render model and the raw source;
  - the authenticated author;
  - the route label, which is a label, never an identity;
  - its retention state: Unread while any of its rows is unread, with `kept` set while any is kept;
  - its reply: present, or a neutral `Unavailable`.
- `connectivity()`, always present: `Unknown` is never shown as `Offline`.
- `session_notice()`, each notice with the `Intent` that resolves it.
- `composer(key)`.
- `trust(peer)`: always "not verified by this client" in Stage 14, since it has no source until Stage 15.
- `diagnostics()`, and `item_diagnostics(key)`: an outbound row's raw failure code. It is kept off `MessageItem` so a view cannot render it by accident (`human-client-ui.md` §12).

**Intents** (`actions(item)` returns only the legal ones):
- `MarkRead`;
- `Keep`, only after read, naming the read or unkeep whose copy it keeps (re-keep within the session, agreed Q6);
- `Unkeep`;
- `Retry` and `Cancel` on pending rows;
- `Send` (`send_draft`);
- `OpenLink`, only on a person's activation of an allowlisted scheme;
- `Reopen`;
- `RecheckStorage`.

**`ViewEvent`** is what a view hands the root: an `Intent`, or a draft edit the root applies with `draft_changed` before it takes the view's events again. It lives here, not in the toolkit crate, so a root that names no toolkit can handle it; `ui-slint` re-exports it.

None touches trust, administration or recovery. Read is a retention act, so `MarkRead` comes only from `conversation_viewed(key, focused: true)`: never on receipt, never from a notification, never while unfocused, and never from `actions()`.

**Labels and errors:**
- `LabelKey` is a closed enum of stable keys. P6's test enumerates it, and no delivery label reads as read, seen or processed. "May have been received" has its own keys.
- `ErrorClass` covers `SendProblem`, `SendError` and `SessionProblem` exhaustively. `EndpointInUse` is its own class (`human-client-ui.md` §12).

**Bounds:**
- Session-only items (read and not kept, terminal outbound) are capped at `SESSION_ITEM_CAP`, oldest evicted first. Nothing else is evicted, since everything else can be re-listed from the store.
- A copy is a second row with the same origin, application id AND envelope. It is attached to the item already shown rather than dropped, so it is counted, read when viewed and unkept like the first, and no store row is ever unreachable. New text under an old id is a new item. Pairs are remembered up to `DEDUP_CAP`.
- A row the store has released (read, unkept, or terminal) is remembered, up to `DEDUP_CAP` of them, so a listing snapshot taken before the release cannot bring it back.
- An outbound update for a row not yet listed or sent is held (latest per row, at most `HELD_UPDATE_CAP`) and applied when the row arrives, so no order is required of the root. `pending_listed` is authoritative: it discards every held update for a row it does not list.

## What it does not do

- **Across a restart, a late duplicate shows again.** Once the first copy was read and not kept, a late duplicate after a restart shows again as unread in Stage 14. Closing it means retaining bounded, content-free (origin, application id) pairs of read messages, which `RETENTION.md` §5 already allows. Plan §18 carries it to Stage 15 ("Carried here from Stage 14").
- **Accessibility is not tested here.** The accessibility-tree bullet of `human-client-ui.md` §13 is `ui-slint`'s (batch 8).
- **No per-peer path state.** Plan §17 (5) carries it to Stage 15.
