# human-chat-v2

HumanChatV2 valid/invalid canonical application messages, including markdown-subset cases and decode-direction compression vectors with the mandatory cap-abort case (ADR-0050).

`human-chat-v2-envelope.json` — 23 verdict vectors over the envelope shape and grammar: version, kind, the 32-lowercase-hex ID forms, `reply_to` including an unresolvable one, the timestamp interval bounds, `from_endpoint`, an ignored unknown field (the schema is OPEN by design), and `text` required and possibly empty.

The markdown subset is deliberately NOT a validity verdict: out-of-subset markdown falls back to plain-text display rather than rejecting the envelope, so subset conformance is a rendering contract for `tests/human-chat`. Decode-direction compression vectors and the mandatory cap-abort case land in this directory with plan §17 batch 3, exercised by `tests/human-chat`: the shared decoder exists; the frozen vectors do not yet.
