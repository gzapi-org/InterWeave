# human-chat

HumanChatV2 conformance (ADR-0050, plan §17 (3)).

**Current status:** Stage 14, active workspace member, test-only.

- `tests/render.rs` — the markdown subset as a rendering contract, through `chat-protocol`'s render model (feature `markdown`):
  - HUMAN-CHAT.md's fixture cases: raw HTML literal, a `javascript:` link inert, a remote image a placeholder, and nesting 17 and a 33-column table falling back without rejecting the envelope;
  - every bound on both sides of its edge: 16/17 levels, 32/33 columns, 256/257 body rows, and input at and past the decoded ceiling;
  - GFM 0.29's own examples for the two admitted extensions, tables (198–205) and strikethrough (491–492).
- `tests/linearity.rs` — render cost stays linear: seven pathological shapes at `n` and `4n` bytes, the larger allowed at most 9× the time (linear is 4×, quadratic 16×).
- `tests/decode_vectors.rs` — the frozen decode-direction compression vectors of `fixtures/human-chat-v2/`, through the shared decoder: a stream decodes to its frozen raw envelope, exactly the ceiling decodes, and the cap-abort case is refused.
- `tests/envelope_schema.rs` — `human-chat/envelope.schema.json` and `HumanChatV2::parse` give every frozen vector the same verdict, and what the crate serializes and `encode_outbound` sends validates.

The encode direction is not fixture-testable, since brotli output is non-canonical; `chat-protocol`'s own tests pin the sender's sizing rule.
