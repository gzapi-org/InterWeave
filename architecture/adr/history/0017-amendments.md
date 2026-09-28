# ADR-0017 — amendment history

### Amendment 2026-09-28 — The method and event catalogue is contract; versions negotiate additively; close is the connection-fatal reply

**Trigger.** Stage 13 opened (plan §16) with the owner's decision that
the IPC request/event vocabulary is machine-readable before the code.
The frame schema had `method` as a free string and only `send-params` as
a params shape; `hello`/`hello_response` sat outside the frame's
classes; the handshake error reply had no shape; "negotiated in hello,
never configured" named no negotiation rule.

**What changed.** The Decision gains: the catalogue as contract (25
`ipc` concepts, `approved`; `method` the closed eleven-name enum,
`request` binding each name to its params, `event` each type to its
data) mirrored by one table in `ipc-protocol` and bound by
schema-agreement tests; `frame` 2.0.0 as one envelope over ten classes;
the negotiation rule (any positive major is a well-formed hello, the
server speaks 2 and answers others `close{VersionIncompatible,
supported}`, minor = min(client, server), minors additive only, an
added or changed field of a closed shape is a major); `close` as the
connection-fatal reply where no request id exists. `LOCAL-IPC.md`
carries the tables and the phase rules (plan §16 (3)–(4)).

**Not changed.** The two-socket topology (ADR-0037), the framing, the
128 KiB ceiling, keepalive, the handshake error precedence.
