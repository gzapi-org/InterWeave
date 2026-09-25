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

