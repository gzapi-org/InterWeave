# human-chat-v2

HumanChatV2 valid/invalid canonical application messages, including markdown-subset cases and decode-direction compression vectors with the mandatory cap-abort case (ADR-0050).

`human-chat-v2-envelope.json` — 23 verdict vectors over the envelope shape and grammar: version, kind, the 32-lowercase-hex ID forms, `reply_to` including an unresolvable one, the timestamp interval bounds, `from_endpoint`, an ignored unknown field (the schema is OPEN by design), and `text` required and possibly empty.

`human-chat-v2-brotli-decode.json` — 3 decode-direction compression vectors (ADR-0050, plan §17 batch 3):
- an envelope whose brotli stream decodes to its frozen raw form;
- exactly the 196,608-byte ceiling, which decodes;
- the mandatory cap-abort case, one byte past the ceiling, which a receiver must abort mid-stream.

The encode direction is not fixture-testable: brotli output is non-canonical. `verify_fixture_vectors.py` recomputes each result independently, and `tests/human-chat` runs them through the shared decoder.

The markdown subset is deliberately NOT a validity verdict: out-of-subset markdown falls back to plain-text display rather than rejecting the envelope, so subset conformance is a rendering contract for `tests/human-chat`.
