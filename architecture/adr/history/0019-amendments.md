# ADR-0019 — amendment history

### Amendment 2026-08-27 — Waiter retention binds from the stage where admission yields

The Decision said, without qualification, that "matching waiters receive the owner result" and that "successful owner completion atomically installs the positive entry before releasing waiters". Stage 6 implements the reservation map and its bound, and does **not** implement that retention. As the amendment was written, `handle_direct` passed `AttachedAsWaiter` to a helper that reads the positive cache, so a waiter attaching while the owner was still in flight found no entry and **was answered `overloaded`**, its response channel dropped. That reply is corrected in the same commit series — the helper now returns `None` and the caller asserts the branch is unreachable rather than inventing a refusal — so the wording above describes the behaviour this amendment responded to, not current behaviour.

The gap was found by review of the Stage 6 exit gate. Two earlier attempts to record it were wrong in the same way: both cited an in-process test as proof of the clause, and that test asserts only that admission RETURNS `AttachedAsWaiter` and enqueues nothing — it never exercises the code that answers the waiter.

The amendment scopes when the rule binds rather than weakening it. Retention presumes an admission that can be interrupted while holding a reservation. Stage 6's admission acquires, resolves, enqueues and releases inside one synchronous call, so no second request can observe an in-flight reservation and the branch is unreachable; a duplicate arriving afterwards is a positive-cache hit, already governed. The rule takes effect at the first stage whose admission yields, which is the local-client IPC boundary.

Two things this deliberately does not do.

It does not bless the behaviour it found. Answering an attached waiter `overloaded` reports exhaustion for a request that was admitted, and is called non-conforming above; an implementation that cannot reach the branch must say so rather than answer it. The runtime was corrected accordingly in the same series.

It does not touch the bound. SPIKE-002/A11 measured the unbounded form accumulating 39 waiters on a single key with zero refusals — a memory-exhaustion path under peer control — and charging waiters against the same per-peer and global budgets as owners, released together, is the fix. That remains mandatory in every stage. The spike reached the waiter path at all only because its harness parks the owner's channel and defers admission by a synthetic 600 ms, which models the yielding admission this amendment names.
