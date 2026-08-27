# ADR-0026 — amendment history

### Amendment 2026-08-27 — Broadcast ingress consumes its own token buckets

The Decision gave direct inbound two token buckets with stated defaults and
then said only that "Broadcast retains bounded best-effort local drop
behavior". Read against the rest of the paragraph that sentence names a
*delivery* bound — the local queue may drop under overload — and no
*ingress* bound at all.

The gap it left: a trusted peer publishing on a joined channel could spend
unbounded decode, signature-verification and dedup work on every node in
the mesh, because the only limits above that path were the transmit
ceiling and the bounded delivery queue. Both bound how much is *held*;
neither bounds how often the work is *done*. Direct's identical exposure
is exactly what the per-PeerId bucket one line above exists to close, and
the Security implications section already reasons about "a
malicious-but-trusted peer" in those terms.

Broadcast now consumes its own per-trusted-PeerId and global buckets with
direct's defaults.

Two things this deliberately does.

The buckets are **separate instances**, not shared with direct. Sharing
would let a broadcast flood exhaust a peer's direct allowance and vice
versa, which converts a bound on one mode into a denial of the other —
and the two modes are required to stay independently functional.

A message over the rate is **dropped before local delivery admission, and
is still reported to the mesh** under the ordinary ADR-0029 mapping. A
validation verdict answers whether a message was structurally valid and
its publisher authorized; a local rate limit answers neither, so
suppressing the report would tell the mesh something untrue about the
message and, under `Reject`, would penalise a relay for this node's own
congestion. GossipSub has no per-message refusal to return to a publisher
in any case.

The delivery-side rule is unchanged: broadcast local delivery may still
drop under overload where direct must refuse before accepting, because
broadcast makes no per-recipient acceptance promise to anyone.

**What the bucket does not bound**, recorded because the first draft of
this note claimed more than it delivers. It said the gap was "unbounded
decode, signature-verification and dedup work". Fingerprinting, dedup and
fan-out are bounded. Signature verification is not: the GossipSub backend
performs it before the runtime is handed the message, so no runtime-layer
limit can precede it. Envelope decoding is not either, and that one is
structural rather than incidental — the mesh is owed an
Accept/Ignore/Reject verdict, the verdict depends on whether the envelope
decodes, so a limit that skipped decoding would have to answer without
knowing whether the bytes were valid. Both are bounded per message by the
transmit ceiling, which is why the remaining exposure is the repeated
cost of hashing a 48 KiB payload and copying it once per joined session —
which is what the bucket closes.
