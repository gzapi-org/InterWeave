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

### Amendment 2026-10-07 — A closed result shape may widen behind a new minor while the old shape is served below it

**Trigger.** ADR-0028 A 2026-10-07 persists admin trust changes, and
`admin.trust.list`'s row (`ipc/trust-list`, `active` since Stage 15's
close) must say `persisted: true` and where a row comes from. The rule
as written made any change to a property of an active closed shape a
major once the first production build spoke 2.0, and the supplier review
of the ADR-0028 text found it cited as its opposite.

**Ruled.** A closed RESULT shape may widen behind a new minor: a property
gains a value, or a new optional property appears, only on a connection
that negotiated that minor or later; below it the shape served is
byte-identical to what the schema described before; the schema describes
both shapes, naming the minor each applies from; the mirror serves by the
negotiated minor. The bound: the previous shape is served on every supported
minor below the new one, so a client built against it never sees the
new one — which is what made the change a major. A change that cannot
keep the old shape below the minor stays a major. First use:
`ipc/trust-list` 1.1.0 behind IPC 2.3 (`persisted` true with `source` on
2.3+, the 2.1 row below).

**Not changed.** Minors additive only; an added, removed or changed
property of a closed PARAMS shape, or a removed property anywhere, is a
major; the pre-release exception of 2026-10-01 for `approved` schemas.

### Amendment 2026-10-07 — The pre-release bound is the first production build for an approved and a flipped schema alike; a pure relaxation is additive before it

**Trigger.** Stage 13's close (#165, 2026-10-01) routed two questions
to the owner: the pre-release clause names an `approved` schema, so a
schema flipped `active` before the first production build was named by
neither the clause nor the major rule; and the clause does not name a
pure relaxation of a bound (the `ipc/hello` 1.2.0 widening of a
claimed-id length). The owner closed both on 2026-10-07 by taking
architect-cto's recommendation: by then the first production build had
spoken 2.0, so the bound question had answered itself, and the same
day's result-shape amendment covered a widening after the build.

**Ruled.** The bound is the first production build whether the schema
was `approved` or flipped `active` before that build; the flip changes
what the schema claims (ADR-0049), not when the wire binds. A pure
relaxation of a bound is additive under the pre-release clause before
the build, when no consumer built against the old bound exists; after
it, a relaxation on a result shape is a widening behind a new minor
(the same day's earlier amendment), and a relaxation on a params shape
is a changed property like any other and a major — a server built
against the old bound would refuse what the new client sends, and
`hello` is sent before any minor is negotiated. (The blind review of
#220 found the first draft let a params relaxation go behind a minor,
against the earlier amendment's "Not changed"; this is the consistent
ruling.)

**Not changed.** The major rule for an added, removed or changed
property after the build; the result-shape widening of the same day.
