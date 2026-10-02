# ui-model

The human client's presentation state (plan §17 (6)). It sits between the transport facade (`crates/human/transport-client`) and the views. It is pure and synchronous, with no facade, no store and no I/O inside:
- the composition root feeds it what the facade and the store did;
- a view renders what it returns;
- a person's actions come back as `Intent`s, so a view never reaches the facade, the store or IPC.

It names the client's vocabulary (`interweave-human-client-api`) and nothing that reaches a store, a transport or a toolkit (plan §17 P2: no `rusqlite`, `libp2p` or `slint`).

**Current status:** active workspace member since Stage 14 batch 6, tested against `tests/local-client-fake` through the facade.

## The contract

Agreed with the client's role before it was built (relay seqs 10630, 10639 and 10642).

**Inputs:**
- the facade's `ClientEvent`s;
- `received` (from `drain`);
- `sent` (after the facade committed a row);
- `pending_listed`, `unread_listed` and `kept_listed` (from the store, at start and on `UnreadInStore`, merged by row id);
- the store acts' results: `read`, `kept`, `unkept`;
- `send_refused` (the composer keeps the draft and shows why);
- `draft_changed`;
- the facade's diagnostics.

**Outputs:**
- `conversations()`: titles never taken from what a peer asserted. A direct route's title is its short `PeerId` and its route label.
- `messages(key)`, ordered by LOCAL time. The peer's `sent_at_ms` is display only. Each item has:
  - a stable `ItemKey`;
  - the render model and the raw source;
  - the authenticated author;
  - the route label, which is a label, never an identity;
  - its retention state;
  - its reply: present, or a neutral `Unavailable`.
- `connectivity()`, always present: `Unknown` is never shown as `Offline`.
- `session_notice()`, each notice with the `Intent` that resolves it.
- `composer(key)`.
- `trust(peer)`: always "not verified by this client" in Stage 14, since it has no source until Stage 15.
- `diagnostics()`.

**Intents** (`actions(item)` returns only the legal ones):
- `MarkRead`;
- `Keep`, only after read;
- `Unkeep`;
- `Retry` and `Cancel` on pending rows;
- `Send` (`send_draft`);
- `OpenLink`, only on a person's activation of an allowlisted scheme;
- `Reopen`;
- `RecheckStorage`.

None touches trust, administration or recovery. Read is a retention act, so `MarkRead` comes only from `conversation_viewed(key, focused: true)`: never on receipt, never from a notification, never while unfocused.

**Labels and errors:**
- `LabelKey` is a closed enum of stable keys. P6's test enumerates it, and no delivery label reads as read, seen or processed. "May have been received" has its own keys.
- `ErrorClass` covers `SendProblem`, `SendError` and `SessionProblem` exhaustively. `EndpointInUse` is its own class (`human-client-ui.md` §12).

**Bounds:**
- Session-only items (read and not kept, terminal outbound) are capped at `SESSION_ITEM_CAP`, oldest evicted first. Nothing else is evicted, since everything else can be re-listed from the store.
- A duplicate (origin, application id) within a session is dropped, remembering `DEDUP_CAP` pairs.

## What it does not do

- **Across a restart, a late duplicate shows again.** Once the first copy was read and not kept, a late duplicate after a restart shows again as unread. Closing that would mean retaining read ids, which is a `RETENTION.md` question.
- **Accessibility is not tested here.** The accessibility-tree bullet of `human-client-ui.md` §13 is `ui-slint`'s (batch 8).
- **No per-peer path state.** Plan §17 (5) carries it to Stage 15.
