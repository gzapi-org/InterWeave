# ADR-0053 — amendment history

### Amendment 2026-09-25 — The record after PR #112 landed: the rebuild's owner, the multipart answer, the backstop's reach

Raised by the last reviews of PR #112 (the automated review of
fbaca241 and the blind reviews of 44754742 and 05f86ce0), relayed by
p2p-network-dev after the merge (67dfb57). The record had been edited
in place while it lived on that branch; these are the first changes
after it landed, so they are the first amendment.

Prior wording, rule 5: "recovery is the runtime's, which rebuilds the
behaviour." Prior wording, rule 4: "which is per record or stricter and
two timestamps per interface." Prior wording, rule 9: "is unreachable
while rule 4 holds — reaching it would need rule 4 switched off, a test
knob of the kind ADR-0052 rule 7 refuses" and "No test seam is patched
into the crate".

What changed and why. The rebuild had no owner: nothing in the tree
rebuilds the mDNS behaviour after the watcher dies, so a node sees no
new interface until the process restarts and the only signal is the
one `MdnsWatcherFailed`; rule 5 now names the rebuild as
p2p-network-dev's, in the next mDNS change with the accessor and the
refresh, and states the interim loss rather than implying a recovery
that does not exist. Rule 4 overstated "per record or stricter": across
answers it holds, but within one multipart peer answer — more than 29
relevant addresses on one interface address — the PTR is repeated in
each of ceil(A/29) packets sent back to back after one rate check, an
upstream behaviour kept on purpose because a receiver reads a packet's
TXT records only through a PTR in that packet; the output is bounded at
one answer and 16 packets per second per interface address, and the
trigger is local configuration. Rule 9 said the send-buffer cap was
unreachable while rule 4 holds; one answer reaches it at 465 relevant
listen addresses, by configuration and without any test knob, so the
sentence now says how it is reached and that it is asserted present
and exercised by no wire test. Rule 9's "no test seam" sat beside rule
8's export of the `Provider` trait; the sentence now says what it
means — no packet-injection path — and that naming an existing trait
adds no code path. The digest's entry, which still described one
event mapped by the wrapper and the stop as owed, follows the record.

Corrected on the same PR (#113), from its blind review, before the
amendment landed: the rebuild is named as re-running
`mdns_driver::build_behaviour`, not as going "through the exported
`Provider` seam" (the export is the test seam and adds no code path,
as rules 8 and 9 say); the interim loss now covers the whole cost of a
dead watcher — no teardown of an interface that goes away either, its
task keeping sockets and rate slots and its failing sends reporting
`InterfaceFailed`; rule 9 no longer says `mdns_bounds.rs` "says so"
while that file still carries the retracted wording, and lists it,
with the vendored crate's two recovery comments, among what the next
mDNS change corrects; the spent revisit condition on the spin is
replaced by one on the rebuild; the digest says per interface ADDRESS
and marks the composer's mapping as owed; `resource-limits.md` says
per answer, not per record.

### Amendment 2026-09-26 — The rebuild needs a Drop: the vendored Behaviour detaches its interface tasks

Raised by p2p-network-dev (GZCoord 01a0dbbd-7650-7cf9-8c05-9143eeea1249,
2026-09-26) while preparing the rebuild rule 5 assigned to them on
2026-09-25, and before building it, as rule 8 requires: the vendored
`Behaviour` has no `Drop` impl (the only `impl Drop` hit in
`third_party/libp2p-mdns/src/` is the `DropCounts` struct's inherent
impl), its interface tasks are tokio `JoinHandle`s (`behaviour.rs:168`)
that detach when dropped, and `abort()` is called only when the
watcher reports `IfEvent::Down` (`behaviour.rs:519-522`). A dropped
behaviour's task therefore runs on: it keeps its multicast socket,
queries every interval and answers with the listen addresses it held,
until its next attempt to hand a discovered pair to the dropped
receiver fails (`iface.rs:343-347`, the `is_disconnected` arm). The
sockets bind with `SO_REUSEPORT` (`iface.rs:160,171`), so the rebuilt
behaviour's task on the same interface binds alongside; each rebuild
adds one more answerer per interface, each with its own rule 4 slot.
Verified by architect-cto in the tree at 59eb6683 before writing; not
yet measured over a namespace — the duplicate answer is inferred from
`SO_REUSEPORT` plus rule 4's per-task slot.

Prior wording, rule 5 (2026-09-25): "recovery is the runtime's —
re-running `mdns_driver::build_behaviour` when `MdnsWatcherFailed`
arrives … and that rebuild is NOT yet built". Prior wording, rule 8:
three items after "Nothing else is patched" — the log discipline, the
stop with the `Provider` export, the accessor.

What changed and why. Rule 5 now states the premise the rebuild rests
on — that the replaced behaviour stops — and that the crate does not
supply it, and adds to the patch `impl Drop for Behaviour<P>` aborting
every `if_tasks` handle; the rebuild is built only on top of it. The
alternative — an explicit `shutdown()` the driver calls before the
swap — was not taken: `Drop` covers every path a behaviour leaves by,
the Swarm's own teardown included, and a method the caller must
remember to call is the shape that produced this defect. Two
measurements are named. The first goes through the `Provider` seam's
`spawn`, not its `TaskHandle`: the `Abort` trait that bounds
`TaskHandle` (`behaviour.rs:145-148`) is declared under
`#[allow(unreachable_pub)] // Not re-exported.` in the private
`behaviour` module, so a test outside the crate cannot implement it for
a recording handle, and a `pub use` of it would be a fifth patch item
this record does not want — the blind review of this amendment caught
the first draft naming that unreachable seam. A test `Provider` whose
`spawn` wraps each task in a future that records its own drop needs no
export: an aborted task's future is dropped at its next scheduling
point and a detached task's is not, so "every wrapped task dropped once
the behaviour is dropped and the runtime has turned" is the abort,
observed. The second is the namespace harness asserting one answer per
query per interface across a rebuild — rule 4's bound holding through
the swap. The same review asked for the swap's order to be stated: the
replaced behaviour is dropped before the new one is first polled, which
a plain assignment already does because `build_behaviour` returns an
unpolled behaviour and tasks are spawned only on `IfEvent::Up` inside
`poll`; rule 5 says so, so the rebuild cannot drift into polling the
new behaviour first and opening an overlap window.
Rule 8 lists the `Drop` as the patch's fourth item. The Implementation
section orders the rebuild on top of it. The digest's D5 and D8
sentences follow.
