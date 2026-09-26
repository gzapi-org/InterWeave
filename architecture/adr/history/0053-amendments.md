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
new behaviour first. The re-review of that fix added two bounds to the
wording: the order guarantees that no replaced task is polled after the
swap, not that no packet of one in-flight poll escapes on a multi-thread
runtime; and the drop-recording test must feed the old task nothing that
would end a detached task anyway, read the drops before shutdown, and be
red with the `Drop` removed — otherwise it passes without the thing it
measures.
Rule 8 lists the `Drop` as the patch's fourth item. The Implementation
section orders the rebuild on top of it. The digest's D5 and D8
sentences follow.

### Amendment 2026-09-26 — The rebuild is built: it runs on the refresh tick, and the duplicate answer is measured

Raised by p2p-network-dev (GZCoord 01a0dbd4-46f2-7fb4-b16a-453c89b39a42,
2026-09-26) after building the Drop and the rebuild on
`develop-qzapp/p2p-network-dev-01/feat/mdns-refresh-rebuild` (a302af30
the Drop, c42dcd4c the rebuild, 708d3ec8 the accessor, 3206f583 the
refresh, db40b386 and e8e417f9 the wording), a branch with no PR at the
time of writing; read there by architect-cto before writing.

Prior wording, rule 5: "that rebuild is NOT yet built (A 2026-09-25,
after PR #112 landed): it is p2p-network-dev's, in the next mDNS change
beside rule 8's accessor and rule 10's refresh." Prior wording, rule 9:
"`mdns_bounds.rs` still carries it in its header and beside its
assertion, and follows in the next mDNS change." Prior wording,
Implementation: "in the next mDNS change, the live-records accessor
(rule 8), the driver's 60 s refresh (rule 10) and the behaviour
rebuild …".

What changed and why. Three facts. First, a choice the record did not
state and now does, as p2p-network-dev built it: the rebuild runs on
the refresh tick, not on the `WatcherFailed` event. A replacement whose
watcher fails at once reports again, and a rebuild per report would
reintroduce, one layer up, the spin rule 5 stopped inside the crate;
one rebuild per `REFRESH_INTERVAL` bounds it and a node sees new
interfaces again within a minute when the rebuild
succeeds. A rebuild whose watcher cannot be
built keeps the running behaviour — it still serves the interfaces it
has, so dropping it would lose them for nothing — stays due for the
next tick, and is reported as `MdnsUnavailable`, the event a watcher
that could not be built at start produces, at most once per tick. The
swap re-tells the fresh behaviour every listen address, because the
Swarm does not repeat `NewListenAddr`; without that the rebuilt
behaviour answered naming no address, which a test mutation showed.
Second, the earlier 2026-09-26 note said of the duplicate answer after a
rebuild: "not yet measured over a namespace — the duplicate answer is
inferred from `SO_REUSEPORT` plus rule 4's per-task slot." It is now
observed:
`a_rebuilt_behaviour_answers_once_and_names_its_listen_address` sees
two answers to one query after the swap with the `Drop`'s abort loop
removed and one with it, and
`a_dropped_behaviour_stops_its_interface_tasks` is red with the `Drop`
removed (0 of 1 tasks dropped) under the conditions rule 5 sets for it: no
discovered pair, no read error, drops read before shutdown.
That sentence of the earlier note stands as written; this paragraph is
the record that the inference became a measurement the same day.
Third, the prose that described the world before the build: rule 5's
"NOT yet built" now dates the assignment and names where it is built;
rule 9's parenthesis says `mdns_bounds.rs` carried the retracted wording
until db40b386; the Implementation section's "next mDNS change" list
names each item's commit. The verbs stop at "built on the branch": the
PR is pending, and "landed" is a merge commit on `main`. The plan's
Stage 11 mDNS bullet names none of these items and needs no change.

The blind review of this amendment found wrong sentences in its first
draft, in the ADR body and in this note. In the body, three still read
as before the build: rule 5's "when `MdnsWatcherFailed` arrives"
beside the new "on the refresh tick", rule 8's "next mDNS change" on
the accessor, and the revisit condition's "once built"; and one was a
wrong cross-reference, the drop test's "meeting the three conditions
above", whose conditions come later in the rule. In this note, "the
Swarm repeats `NewListenAddr` to no one" was a misleading way of saying
what rule 5 says plainly and now reads "the Swarm does not repeat
`NewListenAddr`"; the quotation of the earlier note added "not
observed" to a sentence that does not contain it and is now exact; and
"within one" in rule 5 and "within a minute" here now go on "when
the rebuild succeeds", since a failed rebuild stays due.

### Amendment 2026-09-26 — The rebuild's failure is its own event, and the provider prose follows PR #120

Raised by PR #120's blind review (F8, review 5324544605) and relayed by
p2p-network-dev (GZCoord 01a0dc04-70ef-734b-8418-d329b2a07ed0): the
record's second 2026-09-26 amendment was written against c42dcd4c, where
a rebuild that could not build a watcher pushed
`SwarmEvent::MdnsUnavailable`; that PR's review fixes — F3 and F4, by
p2p-network-dev's account (68f832bf) — made the failure its own event,
`SwarmEvent::MdnsRebuildFailed { detail }`, and left `MdnsUnavailable`
to the start alone. Read at 68f832bf: `messages.rs` (the two variants'
docs), `mod.rs` (`mdns_tick`, the two push sites, the hold behind a full
outbox with the latest winning).

Prior wording, rule 5: "stays due for the next tick and is reported as
`MdnsUnavailable` at most once per tick". Prior wording,
`discovery/providers/mdns.md` line 9: "(ADR-0053 rule 10 — owed in the
next mDNS change; until it lands, a provider fed by discovery events
alone forgets a live peer after 120 s)"; line 37: "Today only a failed
interface watcher does (`SwarmEvent::MdnsUnavailable`, emitted once,
before any other event)."

What changed and why. The distinct event is accepted for the reason rule
5 already gives for `WatcherFailed` beside `InterfaceFailed`: a rebuild
that fails while mDNS keeps serving the interfaces it has is a different
state from "unavailable at start", and a variant is the honest shape
where a reused name would smuggle a meaning into a value. Rule 5 names
the event and its hold; the second 2026-09-26 note's sentence stands as
written, this note being the record that the event changed after it.
Every "PR pending" now names PR #120, open at the time of writing —
"landed" waits for the merge commit. The provider document follows: rule
10 is built (PR #120), and the degraded signal has four producers — the
start's `MdnsUnavailable`, each interface's `MdnsInterfaceFailed` (held
one per interface, the latest reason winning), the watcher's
`MdnsWatcherFailed`, the rebuild's `MdnsRebuildFailed`; the first draft
of this amendment counted three and its own paragraph named the fourth,
which the supply's blind review caught, together with the Operational
section's sentence that still sent bind, join, send and receive failures
to `MdnsUnavailable` — it now lists the four. Rule 5's composer sentence
names `MdnsRebuildFailed` among the events Stage 12's composer reads.
Supplied onto PR #120 as architect-cto's commit, cut from its head with
`origin/main` folded, because a record naming one event while the code
emits another must not sit on `main` between two merges.

### Amendment 2026-09-26 — The mDNS set landed; two holds the code keeps across a rebuild

PR #120 merged at 53cce603 (head 795c0fc0), armed on the owner's word
after four review-class rounds with no open P1 or P2, carrying the
Drop (a302af30), the rebuild on the refresh tick (c42dcd4c), the
accessor (708d3ec8), the refresh (3206f583), the wording fixes and
architect-cto's supplied third amendment (ce2b9dce). Reported by
p2p-network-dev (GZCoord 01a0dc0f-73ec-7f28-ab4a-d3d8c37889c4 and
01a0dc25-e0c1-7248-956b-90c8bdcd2265); read at 53cce603 before
writing.

Prior wording, rule 5: "PR #120, open at the time of writing", "Until
that PR lands, a node that loses its interface watcher sees no new
interface", and "held behind a full outbox with the latest winning".
Prior wording, rule 7: the rule ended at "(ADR-0052 rule 5's discipline
holds for a store the boundary sits behind)." Prior wording, rule 8:
"(built on `feat/mdns-refresh-rebuild`, 708d3ec8, PR #120)". Prior
wording, Implementation: "PR #120, open at the time of writing" and
"All p2p-network-dev's, on their branch, in rule 9's order; this record
is the caller and lands on that branch ahead of the patch." Prior
wording, Revisit conditions: "once it lands".

What changed and why. The verbs: "open at the time of writing" reads
landed, with the merge commit, since landed means a merge commit on
`main` and nothing less. Two behaviours built after the third
amendment, both of the class that amendment named as omissions: a held
`MdnsRebuildFailed` is dropped unsent when a later rebuild succeeds
(`MdnsState::rebuilt` clears it), because a failure reported after the
success would describe the opposite of the state it arrives in — the
test is `a_successful_rebuild_drops_a_held_report_of_an_earlier_failure`
— and it is held behind an older hold as well as a full outbox; and the
runtime's drop-count cell keeps the last replaced behaviour's counters
live until the next replace, because the `Drop` aborts a retired
behaviour's interface tasks but a task already mid-poll on another
worker finishes that poll and may count after the hand-over, so a
snapshot taken at the hand-over would lose the increment — PR #120's
automated-reviewer P2, the test
`a_replaced_behaviours_late_counts_are_still_read`. Both are stated in
the rules they refine (5 and 7); the third note stands as written.

Corrections made to this touch under review, listed as facts about the
text: the revisit condition's "once it lands" and the Implementation
section's "on their branch … lands on that branch" read landed; the
provider document's "until ADR-0053 lands" reads landed with PR #112,
and its "are unbounded" reads "were unbounded in 0.49.0 as released";
the provider document's hold wording gained "or an older hold" and
"dropped unsent when a later rebuild succeeds", as rule 5 has; rule 7's
"no drop goes uncounted" reads "every increment made before the next
replace is read"; "Before that landed" names PR #120 and is in the past
tense; this note's prior-wording paragraph quotes rule 5's phrase once,
rule 7's real last sentence and rule 8's prior parenthesis; and the
Implementation section's account of order went through three forms —
"landed ahead of the patch each time" (false for #112: one merge,
67dfb571), "reached the branch ahead of the patch each time" (false for
#120: its first eleven patch commits, 708d3ec8 through e8e417f9,
predate #119's and #121's arrival on that branch through a56f908e and
fbe8edc9; its last four follow them) — and now says the record reached
`main` no later than the patch, which git shows for both.
