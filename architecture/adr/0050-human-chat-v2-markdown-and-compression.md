# HumanChatV2 carries markdown natively, compresses only to fit, and bounds decompression

**Status:** Accepted (2026-08-17). Supersedes the HumanChatV1 envelope design in `clients/human/HUMAN-CHAT.md` **before any implementation exists**, the same pre-implementation supersession `/interweave/direct/2.0.0` applied to `/direct/1.0.0` (ADR-0005/0030); there is no deployed v1 compatibility obligation. Amends the application-identifier list of ADR-0047 to the v2 media type.

## Context

HumanChatV1 (`clients/human/HUMAN-CHAT.md`) is a plain-text envelope: `kind: "text"`, active markup explicitly unsupported, attachments out of scope. Three pressures have emerged against that shape:

- **The consumers are documents, not only chat lines.** The transport payload ceiling is 49,152 bytes (`contracts/TRANSPORT.md`); measured against this repository's own corpus, the mean architecture document is ~6 KB and the largest single document (46,932 bytes) fills 95% of a payload raw but 30% compressed. Useful document exchange sits near the ceiling exactly where plain text has no headroom.
- **AI agents are first-class endpoints.** The Claude bridge (`plugin/CLAUDE-CODE-CHANNEL.md`, ADR-0002/0023) hands payload content into a model's context. Markdown is the format such consumers read natively — no lossy render-to-text step — and the same source renders for humans. One format serving both consumers removes a conversion layer that would otherwise need its own correctness rules.
- **Compression is only sometimes worth it.** Measured with brotli at quality 11 on realistic short messages, inputs under roughly 60–80 bytes *expand* (a 19-byte message becomes 23 bytes), while per-document compression of markdown in the 4–8 KB range achieves ~2.7–2.9×. Compressing everything buys nothing on the dominant short-message case and puts a decompression step — and its attack surface — on every message.

The dangerous property of that attack surface was measured, not assumed: 48 KiB of hostile brotli input expands to at least 4 GiB (≥87,381×; the measurement hit its own cap, the true bound is higher). Any design that decompresses remote input must carry an explicit output bound enforced independently of anything the peer declares.

Assumption stated per template: the compression measurements above are from this repository's markdown corpus (150 documents) and short synthetic chat messages; no live user message distribution exists yet to calibrate against.

## Decision

1. **HumanChatV2 replaces HumanChatV1 as the implementation target.** Media type `application/vnd.interweave-human-chat+json;v=2`. The v1 media type is not implemented and not a compatibility alias. Envelope identity fields (`app_message_id`, `reply_to`, `sent_at_ms`, `from_endpoint` and their grammars and trust semantics) carry over from v1 unchanged; the schema remains OPEN for forward compatibility within v2.

2. **`text` is markdown.** The content of `kind: "text"` is **CommonMark 0.31.2**, plus exactly two extensions taken from the GitHub Flavored Markdown specification (version 0.29-gfm): its `table` and `strikethrough` grammars. Both the core version and the extension grammars are pinned because neither is a single agreed dialect — implementations disagree about whether the same source is a table, a strikethrough, a link destination, or literal text, and that disagreement lands *before* the security rules below can apply. No other GFM extension is included: task lists, autolinks, and footnotes are outside the subset and render as literal text. The bounds live in the v2 revision of `clients/human/HUMAN-CHAT.md`. Non-negotiable subset rules, wire-visible and consumer-visible:
   - raw HTML is never parsed or rendered as HTML — it is displayed as literal text;
   - link schemes are allowlisted (`https`, `mailto`); a link in any other scheme renders inert as plain text;
   - referenced remote images are **never fetched automatically** — a human client renders an explicit user-triggered placeholder, an agent-facing consumer treats the reference as inert data;
   - nesting depth and table dimensions are bounded (values in the v2 specification), and render cost must stay linear in input size;
   - the raw markdown source is always available to the receiving user, so what was sent is always inspectable independently of how it rendered.

3. **Plain rendering is legal degradation.** A minimal client may present the markdown source as plain text. This is markdown's design property and requires no capability negotiation, version handshake, or fallback field.

4. **Compression is a fit fallback, never a default.** The sender MUST send the envelope as raw UTF-8 JSON whenever that fits the effective transport payload limit (`max_payload_bytes`). Only when the raw encoded envelope exceeds that limit MAY the sender send the whole envelope brotli-compressed (RFC 7932), signalled by the media-type parameter `ce=br` (`application/vnd.interweave-human-chat+json;v=2;ce=br`). If the compressed form still exceeds the limit, the message is too large: there is no chunking, no multi-message reassembly, and no automatic downgrade. Receivers accept both forms regardless of size; the constraint binds the sender.

   **A raw envelope larger than the rule-5 decompressed ceiling is too large before compression is considered.** Compressibility does not extend the ceiling: a highly repetitive 300 KB document brotli-compresses well under `max_payload_bytes`, but every conforming receiver aborts it mid-decode, so permitting the send would define a message that is sender-conforming and universally unacceptable. The sender's legal range for compression is therefore `max_payload_bytes < raw <= 196,608`.

5. **Hard decompressed ceiling: 196,608 bytes** (4 × the 49,152-byte transport payload ceiling). This ADR sets that number. The receiver MUST stream-decode with this cap and abort mid-stream as soon as the output **would exceed** it: 196,608 bytes decode successfully and the 196,609th aborts. The cap is a legal size, not the first illegal one — aborting *at* it would make an envelope of exactly 196,608 raw bytes sender-conforming under rule 4 and refused by every conforming receiver, which is the same gap rule 4 exists to close. There is deliberately **no declared uncompressed-length field**: a declared length would be peer-asserted metadata the cap must override anyway, so it adds an oracle and no safety. The multiplier is 4× because the measured achievable brotli ratio on real markdown is ~3×, so the cap never rejects a payload that legitimately fit under rule 4, while bounding hostile expansion at four payloads.

6. **Decompression happens above transport, once.** The daemon never decompresses: payload bytes stay opaque to transport (`contracts/TRANSPORT.md`) and the bomb guard never enters the daemon's admission path. One shared application-layer library implements decode-with-cap and subset validation for the desktop client, the Android client, and the Claude bridge. The bridge's defense-in-depth checks (`plugin/CLAUDE-CODE-CHANNEL.md`) run on the **decompressed** bytes — a size check calibrated against compressed input would be smuggled past by rule 4's own mechanism. `contracts/CHANNEL-EVENT.md` states the corresponding bridge rule: a content-encoding parameter is decoded before content classification, which is representation and not the application-protocol parsing that contract forbids. Without it a compressed envelope would satisfy the non-UTF-8 rule and reach the model as opaque base64url.

7. **Dedup and fingerprint semantics are unchanged.** `DirectContentFingerprintV1` remains computed over the wire payload bytes as sent. Because brotli output is not canonical — two encoders, or two versions of one encoder, legitimately produce different bytes for the same input — an application retry MUST resend the stored byte-identical payload, never re-encode. Re-encoding on retry can produce a same-key/different-fingerprint conflict that ADR-0019 correctly rejects.

8. **Peer content is data, not instructions — for both consumer kinds.** A human client renders inside the rule-2 subset with raw source viewable. An agent-facing consumer MUST deliver peer content framed as untrusted data attributed to `source_peer`/`source_endpoint`, never concatenated bare into model context, and MUST NOT automatically follow links or fetch resources named in received content. The containing boundary is ADR-0032's rule that network content never invokes administrative capability: a successfully prompt-injected agent still holds only its data-plane endpoint lease.

9. **Retention and scope are unchanged.** ADR-0044 and `clients/human/RETENTION.md` apply to v2 exactly as to v1; compression is representation, not semantics, and carries no retention, receipt, or persistence request. Attachments, binary media, and out-of-band artifact references (for example git-hosted content) remain **out of HumanChatV2** and require their own ADR.

## Alternatives considered

Keep plain text (v1 status quo) — loses the document use case and forces every AI consumer through a formatting layer anyway. HTML or a rich-text subset — an injection surface with no benefit over markdown for either consumer. Always-compress — expands the dominant sub-80-byte case and puts the bomb guard on every message for savings that are irrelevant at 0.7% of the ceiling. Per-field compression inside the JSON envelope — base64 adds 33% to the compressed bytes and forces two parse paths. gzip/zstd/xz instead of brotli — measured per-document on this repository's corpus: brotli 2.94×, bzip2 2.44×, zstd 2.42×, gzip 2.38×, xz 2.34×; brotli's built-in static dictionary is decisive at these document sizes, where LZ77 history never warms up (xz, the large-file winner, places last). A trained shared dictionary — +14% over plain brotli in held-out measurement, but it is versioned shared wire state needing distribution, freezing, and a compatibility story; rejected as cost without need. Compressing at the transport layer — breaks payload opacity and moves hostile decompression into the daemon. Raising the 48 KiB transport ceiling — breaks the IPC payload-fit invariant (`contracts/LOCAL-IPC.md`) and is a transport-wide change to serve one application protocol. A declared uncompressed-length field — see rule 5. Chunked multi-message documents — reintroduces the reassembly/session state the transport deliberately does not have (ADR-0018/0020).

## Consequences

One content model spans a chat line, an inline document, and any future referenced artifact: `text/markdown` semantics differing only in size and delivery. The Claude bridge's `media_type` → `content_type` mapping stays one-for-one. Effective document capacity rises from 49,152 to roughly 144,000 source bytes (measured ~2.9× on real markdown) without touching any transport constant.

Costs: every first-party consumer gains a markdown subset validator and a bounded brotli decoder (one shared library, but it must exist before the human application layer opens); encode-direction compression is not fixture-testable because brotli output is non-canonical, so conformance vectors pin the decode direction only (compressed bytes → expected output, and cap-violation inputs → mandatory abort); the v1 fixture set planned in the bottom-up plan (`fixtures/human-chat-v1/`) is replaced by a v2 set before it was ever materialized.

Acceptance propagated in the same commit series: `clients/human/HUMAN-CHAT.md` is rewritten to v2 and carries the pinned subset bounds; `contracts/schemas/human-chat/` and its manifest are updated (ADR-0049, still `approved` — nothing implements it); the fixture table, phase rows, and track lists in `architecture/roadmap/BOTTOM-UP-IMPLEMENTATION-PLAN.md`, the human-client explanatory documents, and `fixtures/human-chat-v2/` (renamed from the never-materialized v1 directory) all reference v2; ADR-0047's application-identifier list is amended to the v2 media type. The frozen `fixtures/direct-v2/` fingerprint vector that uses the v1 media-type string as sample input bytes is deliberately untouched — those are arbitrary test bytes inside a frozen vector, and changing them would be a protocol event with no protocol reason.

## Security implications

- **Prompt injection** is the primary new exposure and is contained, not eliminated: any text a model reads is instruction-shaped, so rule 8 mandates provenance framing, and the blast radius is bounded by the pre-existing authority split (ADR-0032/0037) — no admin IPC, no trust mutation, no key material behind a data-plane endpoint.
- **Decompression bombs** are bounded by rule 5's streaming cap. The measured ≥87,381× expansion is exactly why the cap is enforced mid-stream and independent of any declaration.
- **Render injection** is closed by rule 2: no HTML passthrough, scheme allowlist, no active content.
- **Read-beacon exfiltration** is closed by never auto-fetching: for a human client a remote image fetched on receipt leaks IP, presence, and read timing (silently defeating ADR-0044's read-state model); for an agent, auto-fetching URLs from received content is an automated exfiltration channel. Same rule, both consumers.
- **Media-type spoofing** stays where v1 left it: `media_type` is advisory (`contracts/TRANSPORT.md`), so a hostile mislabel yields a local malformed-envelope rejection, not a routing or authority change.
- Residual risk accepted: prompt injection within the data plane (an injected agent can still send/broadcast within its lease); the compression path reveals, coarsely, that a message was large — inherent in any size-triggered mechanism.

## Operational implications

Nothing new to configure: the compression threshold is the profile's existing `max_payload_bytes`, the decompressed ceiling is a constant. Visible failure modes: an oversized document fails at the sender with an application error stating that the compressed form was tried and did not fit (no partial delivery exists); a cap-violating or malformed compressed payload is rejected locally by the receiving application with the message dropped and the source attributable. Operators reading agent transcripts see raw markdown with provenance framing, not rendered output.

## Implementation implications

Lives entirely above transport, in the human application layer of ADR-0045's layout: the envelope codec, subset validator, and bounded decoder belong in the shared application-protocol crate (`human-chat-protocol` in `docs/architecture/human-client-cross-platform.md`), consumed by desktop, Android, and the Claude bridge; conformance cases land in `tests/human-chat`; decode-direction and cap-abort vectors in `fixtures/human-chat-v2/`, recomputed by `tools/checks/verify_fixture_vectors.py` per ADR-0049. Everything here is inert until the stage that opens the human application layer (ADR-0046); the only Stage-0-visible effect is that the outstanding human-chat fixture set is authored against v2 instead of v1. No daemon, IPC, or transport change is required — which is the point of rules 4–6.

## Revisit conditions

- An attachment/artifact-reference protocol is decided (its ADR must revisit rule 9's scope line and may reuse rule 8's no-auto-fetch framing).
- A measured real message-size distribution shows the 4× decompressed ceiling either rejecting legitimate compressible documents or being needlessly generous.
- A target platform lacks a maintainable brotli decoder, which would reopen the zstd alternative with its measured ~0.5× per-document penalty.
- The markdown subset proves insufficient for the human clients (e.g. a genuine need for math or diagrams), which is a v3 envelope question, not an extension-field question — v2's OPEN schema ignores unknown fields but rule 2's subset is closed.

## Amendments

Full notes: [`history/0050-amendments.md`](./history/0050-amendments.md).

| Date | Amendment | Effect |
|---|---|---|
| 2026-08-21 | Decoding aborts when the output would exceed the ceiling, not when it reaches it | Rule 5 matches rule 4's inclusive `<= 196,608`, so exactly the ceiling is decodable |
| 2026-08-17 | Bridge decodes content-encoding before classifying content | Rule 6 cites the reconciling rule in `CHANNEL-EVENT.md`; a compressed envelope no longer reaches a model as opaque base64url |
| 2026-08-17 | Raw envelopes over the decompressed ceiling are too large before compression | Rule 4 bounds the sender's compression range at 196,608 raw bytes, closing a sender-conforming/universally-unacceptable gap |
| 2026-08-17 | Markdown dialect pinned to CommonMark 0.31.2 with two named GFM extensions | Rule 2 names exact grammars, so clients cannot disagree about what a construct is before the security rules apply |
