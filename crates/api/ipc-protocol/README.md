# ipc-protocol

IPC v2 frame codec, handshake, and error models.

**Current status:** Stage 1, active workspace member. Wire models only — no socket, stream, async runtime, or platform type.

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
