# ipc-protocol

IPC v2 frame codec, handshake, and the typed mirror of the IPC catalogue.

**Current status:** active workspace member since Stage 1 (the codec and the handshake); Stage 13 (B1) added the typed mirror of the approved `contracts/schemas/ipc` catalogue: `Frame` over the ten classes, version negotiation, the method table, `Request` and its admission, the results and the event catalogue. Wire models only — no socket, stream, async runtime, or platform type.

## Why there is no I/O here

Language-neutral by contract (ADR-0039). This crate decides what the *bytes mean*; carrying them is someone else's job. That split is what lets the desktop daemon and an independent third-party client agree without sharing a transport implementation.

## The decoder never allocates on a declared length

The four-byte prefix arrives from the other side of a socket, so it is untrusted input **even locally**: an owner-protected socket bounds who may connect, not what they send once connected. `decode_frame` checks the declared length against the 131,072-byte ceiling *before* consulting the buffer, and returns `Incomplete { needed }` rather than reserving anything. A decoder that reserved 4 GiB because a peer said so would have conceded the resource the bound exists to protect — there is a test that hands it `u32::MAX` with four bytes of input.

`consumed` on a successful decode is what lets a stream reader advance without the codec holding any stream state of its own.

## Requested is not granted

`RequestedCapability` and `DataCapability`/`AdminCapability` are different types. A client may *ask* for `admin.shutdown` — refusing is the server's job — but nothing can assign a request into a grant, because the types do not convert implicitly.

## The socket is authority; the frame is data

`AuthorityDomain` is supplied by the accepting code from the listener the connection arrived on, never parsed out of the frame. A client claiming `client.kind = "admin"` on the data socket is still on the data socket, and `Hello::evaluate` refuses `admin.*` **categorically** rather than filtering it out silently — so a client can never believe it received an authority it did not (ADR-0037).

Two related rules land in the same place: an admin connection may not claim an endpoint, and a lease claim without negotiated keepalive is denied *at claim time* rather than granted and revoked a moment later — a lease that exists for one round trip is a lease no other client could take.

`tests/schema_agreement.rs` holds all of this to `ipc/hello.schema.json` and `ipc/capability.schema.json`, and cross-checks the frame ceiling against the frozen `fixtures/ipc-v2/ipc-v2-payload-fit.json` vectors — including that the codec emits the exact length prefix each vector recorded.

## The catalogue is contract; the Rust mirrors it

Every method, event, frame class, result and error code is an `approved` schema in `architecture/contracts/schemas/ipc/` first (plan §16 (3)), and this crate mirrors it:

- `Method` is the closed eleven-name catalogue, and `Method::entry` is the ONE table of method → authority domain → required capability → minor. The capability a method needs is not in the schema (the contract meta-schema admits no such annotation), so it lives in that table and in `LOCAL-IPC.md` §Method catalogue, and a test binds the two.
- A request's envelope keeps the method as TEXT and the params as the bytes that arrived, because an unknown method (`ProtocolUnsupported`) and malformed params (`InvalidArgument`) are answers on a connection that stays, not frames that failed to parse. `RequestFrame::admit` judges in the contract's order: the name, the negotiated minor, the other domain's method (counted as cross-domain), the capability, then the params — authority before shape.
- `negotiate` answers any positive major: an unsupported one is a well-formed hello answered `close{VersionIncompatible, supported}`. A version number deserializes saturating, since the schema bounds it below only.
- Envelope fields stay raw (`serde_json`'s `raw_value`), so a frame re-encodes byte-exact without `preserve_order`, which nothing in the workspace enables.

`tests/schema_agreement.rs` binds every vocabulary both ways, reading the enums' variants from their serde derives rather than from a hand-typed list; validates every frame, request, event and result this crate emits against the frozen schemas with `jsonschema`, each beside a control the validator refuses; names every `ipc/*` schema; round-trips the golden frames of `fixtures/ipc-v2/ipc-v2-frame-golden.json` byte-exact; and checks that the largest legal payload fits under the ceiling with its whole envelope.
