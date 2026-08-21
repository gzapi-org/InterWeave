# ADR-0050 — amendment history

### Amendment 2026-08-21 — Decoding aborts when the output would exceed the ceiling, not when it reaches it

Rule 4 gives the sender the inclusive range `max_payload_bytes < raw <= 196,608`, while rule 5 required the receiver to abort "when it is reached". An envelope of exactly 196,608 raw bytes was therefore sender-conforming and refused by every conforming receiver — the identical sender-conforming/universally-unacceptable gap the 2026-08-17 amendment closed at the other end of the range, reopened at one value by the boundary wording.

There was no coherent behaviour to implement: the two rules disagreed about a single size. `crates/human/chat-protocol` had already resolved it in rule 4's favour — its decoder accepts exactly the ceiling and refuses the next byte — so the prose was the part that was wrong.

Rule 5 now aborts as soon as the output *would exceed* the cap. 196,608 bytes decode; the 196,609th aborts. The ceiling stays 4 × the 49,152-byte transport payload ceiling, and that transport ceiling is inclusive too, so the multiplier now means the same thing at both ends.

### Amendment 2026-08-17 — Bridge decodes content-encoding before classifying content

Rule 6 required the Claude bridge's defense-in-depth checks to run on decompressed bytes, but `contracts/CHANNEL-EVENT.md` independently required non-UTF-8 payloads to be forwarded as base64url and stated that the bridge "does not parse JSON/application protocols to infer meaning". A brotli-compressed envelope is non-UTF-8, so a bridge conforming to that contract would forward opaque base64url and never invoke the decoder, while a bridge following rule 6 would violate the event contract. Two accepted documents in conflict is precisely what must not be resolved silently in code (`CLAUDE.md` §2).

`CHANNEL-EVENT.md` now states that a content-encoding parameter is decoded before content classification, and that decoding is representation rather than the application-protocol parsing it forbids — the bridge still reads no envelope field to infer meaning. `meta.content_type` reports the media type with the encoding parameter removed. Rule 6 cites that rule rather than restating it.

### Amendment 2026-08-17 — Raw envelopes over the decompressed ceiling are too large before compression

Rule 4 permitted compression whenever the raw envelope exceeded `max_payload_bytes`, with no upper bound on the raw size, while rule 5 required every receiver to abort decoding past 196,608 bytes. A highly compressible raw envelope above that ceiling therefore satisfied the send rule and could not be accepted by any conforming receiver.

Rule 4 now classifies such an envelope as too large before compression is considered, making the sender's legal compression range `max_payload_bytes < raw <= 196,608`. Compressibility does not extend the ceiling; the prior wording implied it could.

### Amendment 2026-08-17 — Markdown dialect pinned to CommonMark 0.31.2 with two named GFM extensions

Rule 2 promised an exact grammar and then named "CommonMark, plus the table and strikethrough extensions". CommonMark is versioned, and tables and strikethrough are not part of it — they are extensions whose grammars differ between implementations. Clients could therefore disagree about whether a given source is a table, a strikethrough, a link destination, or literal text, and that disagreement occurs before the security and dimension rules can be applied to the parse.

The rule now pins CommonMark 0.31.2 and the `table` and `strikethrough` grammars of GFM 0.29-gfm specifically, and states that no other GFM extension is in the subset.
