# client-api

The human client's caller-facing vocabulary: what the transport facade (`crates/human/transport-client`) says, and what the UI model (`crates/human/ui-model`) reads. It holds the outbound status, the send and session problems, connectivity, the session state, a received message's origin, and the event queue's events.

Types only: no I/O, no store, no transport. It depends on `human-core` (the row and message ids), `chat-protocol` (the envelope, default features) and `transport-api` (the identifiers). It reaches no `rusqlite`, `libp2p` or `slint`, so the UI model can name it (plan §17 P2). architect-cto placed it here on 2026-10-02 (relay seq 10633), rather than writing two copies of one vocabulary or moving it into `human-core`, which must stay unable to see a sender.

**Current status:** active workspace member since Stage 14 batch 6. The contract it encodes is the facade README's "The contract", agreed with the client's role.
