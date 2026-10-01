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

### Amendment 2026-10-01 — Before the first production build, an approved schema takes an additive property and may drop a never-emitted one

**Trigger.** #160 (the Stage 13 ledger audit) moved `ipc/admin-status` to 1.1.0: it added the closed `ingress` block and REMOVED `pre_auth.tracked_peers`, a member `AdminStatusResult::new` always set to `None` and serialization always skipped — never on any wire, and there is no release tag. The addition followed `LOCAL-IPC.md`'s pre-release clause (775c3139, A 2026-09-30, for `event_queue` on `hello_response`); no text covered the removal, and the Decision's sentence "an added or changed field of a closed shape is a major" recorded no exception for either. p2p-network-dev raised it (GZCoord 01a0f778) after #160's review and a second review dispatch judged it a document gap with no code defect.

**Ruled.** The closed-shape major rule binds once the first production build speaks 2.0 — one bound for the rule and its exception, not "on a wire", which development and end-to-end builds already are. Before it, an `approved` schema (ADR-0049: an implementation target, "never a claim that anything implements it") takes an additive property into 2.0 itself, may remove a property no build has ever emitted, and treats a change as that removal plus that addition; its own version moves 1.x → 1.(x+1) and its Rust mirror refuses the old name, so a frame that still carries it is refused on read (`ipc-protocol`'s result test `a_directory_and_a_status_read_back_as_the_port_gave_them` pins it for `tracked_peers` through `deny_unknown_fields`), while the schema-agreement test refuses a mirror that would still emit it. "Property" is a member of a closed object; the method and event catalogues keep their own rule — a removal or rename there is a major at any time (`method.schema.json`; `event.schema.json` says only "minors add types", the removal half resting on LOCAL-IPC.md's "minors are additive only"). `LOCAL-IPC.md` §Version negotiation carries the same sentence; the schema's description names the removal, as `hello_response`'s names its addition. After the first production build, the sentence binds as written and either change is a major.
