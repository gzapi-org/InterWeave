# ADR-0011 — amendment history

### Amendment 2026-09-20 — Discovery never writes the address book

Raised by p2p-network-dev while building the mDNS multicast mechanism
the owner ordered on 2026-09-20 (plan §14). `libp2p-mdns 0.49`'s
behaviour pushes `ToSwarm::NewExternalAddrOfPeer` for every discovered
(peer, address) pair, unconditionally (behaviour.rs:336-344); the Swarm
forwards it to every behaviour (libp2p-swarm 0.48 lib.rs:1165-1172). Any
host on the multicast domain could therefore place an address for any
PeerId into the Swarm. `providers/mdns.md` already says mDNS grants zero
trust and ADR-0012 says discovery never mutates trust, but nothing said
in terms that a provider may not write the address book — the
enforcement had never been needed, because no enabled behaviour consumes
the event today (kad, gossipsub, identify, autonat, relay, dcutr and the
vendored autonat: zero consumers, read at libp2p 0.57).

The decision makes the reading a rule so it binds the next provider and
the next library version: the emission is swallowed at the wrapper, the
only path from a discovered pair to a dialable address runs through the
provider's normalization, bounds and dedup into `DiscoveryManager` and
then ConnectionManager admission, and a multicast conformance test
asserts a discovered pair reaches the pipeline and nothing else
(`DISCOVERY-CONFORMANCE.md`, 2026-09-20). Nothing shipped at the time of
this amendment: `mdns` was enabled on p2p-network-dev's branch and the
behaviour not yet constructed.

### Amendment 2026-09-20 — Identify's advertised addresses enter the book through ADR-0052's boundary

Written on 2026-09-25: the subsection landed in §Implementation
implications on PR #111 (commit a9d4779) beside the DNS transport that
made the gap visible, and the three-part record ADR-0048 requires was
not written with it — the blind re-review of the PR named the omission
(P3-13) and architect-cto, who assigned the paragraph, adds the record.

What changed and why. An Identify `listen_addr` is peer-supplied in
ADR-0052 rule 1's own words and the address book is what the retry
scheduler dials from unprompted, so the path is an instance of ADR-0052
rule 8 as amended that day. It had gone without a hook because nothing
about it looked like a dial and because a `/dns4` listen_addr failed
`MultiaddrNotSupported` by accident of a TCP-only Swarm; building the
DNS transport removed the accident. The instance: the floor, rule 2's
no-name clause included; rule 3 admitting a private address beside a
non-loopback listener of the same family; no rule-4 clause, because a
NAT'd peer legitimately advertises an address that differs from the
one it was observed from. Enforced at the learn site, so a refused
address never becomes an entry and a later relaxation of the retry path
cannot launder one; counted by class, never logged. Nothing else in
the record moved.

### Amendment 2026-09-25 — The Identify hook is a store hook; the crate's address cache is disabled

Raised by the blind re-review of PR #111, which found that
`libp2p-identify` 0.48.0 keeps its own cache of every peer's
`listen_addrs` (default one hundred entries) and returns it from its
pending hook to any dial that extends its addresses through the
behaviours — a second, unfiltered address store beside the book this
record had just put inside the boundary.

What changed and why. The subsection now says what its hook is and is
not: a STORE hook in ADR-0052 rule 8's terms as restated the same day —
it keeps the book clean, which matters because a stored address is
later dialled — and not the dial's enforcement, which is ADR-0052 rule
5's root funnel, the class counterpart of this record's root-level dial
gate. The crate's cache is disabled (`with_cache_size(0)`) so the book
is the only address store; the runtime keeps its own filtered book and
the cache duplicated it without an owner. Both land on PR #111.

### Amendment 2026-09-25 — The Identify instance admits a peer's own circuit address

Raised by the DNS-focused blind review of PR #111: `is_punchable_address`,
applied at the Identify learn site, refused every `/p2p-circuit` address
as `Relayed`, so a NATed peer's advertised relay address never became a
book entry and `DialPeer`'s circuit fallback had nothing for a peer
learned only through Identify. ADR-0052 (A 2026-09-25, "A peer's own
circuit address enters the book through the boundary") scoped rule 2's
no-circuit clause to the dial-back and the punch; this record states
the book's instance: the circuit's transport prefix, through the
relay's `/p2p` component, meets the floor as a literal or is an
operator address, the entry counts against the per-peer cap, and the
relay's own admission is the gate's at dial time — class and
authorization kept apart as ADR-0052 rule 1 requires. The hook is
p2p-network-dev's, on PR #111.

### Amendment 2026-09-25 — Two doors, several stores

Raised by the mDNS-focused and the general blind reviews of PR #111
(F1 and P3-4). Two sentences in this record were false. §Decision said
no enabled behaviour consumes `FromSwarm::NewExternalAddrOfPeer`;
`libp2p-request-response 0.30.0` does, into its `PeerAddresses` cache
(`lib.rs:837`, `libp2p-swarm 0.48.0` `peer_addresses.rs:26`), read in
the pinned source by p2p-network-dev. And the swallow closed one door of
two: `libp2p-mdns 0.49.0` also answers the Swarm's pending-dial hook with
every address multicast named for the dialled peer, which the Swarm
appends to any dial that extends through the behaviours — so until PR
#111 commit f85dd27 a LAN announcement reached Kademlia's, the relay
client's and request-response's dials through the wrapper that was
meant to keep it out. The wrapper now answers that hook with nothing,
with a test beside the unwrapped crate's answer as the control. The
2026-09-25 note above said "the book is the only address store"; that
sentence now reads as the only store the runtime writes from Identify,
and §Identify names the others in the process with their doors.

### Amendment 2026-09-28 — The book remembers the proof: an address's dial state is its book entry's, bounded by the book, never pruned apart

**Trigger.** Stage 12's discovery composition (InterWeave #137) is the
first production feeder whose addresses churn, and the review class
failed the address book's eviction rule five rounds running. The root
cause was structural: the book (`ConnectionManager.book`, a set of
address strings per peer, at most eight) and the policy table
(`ConnectionPolicy`, bounded, pruned after an idle hour and evicted
least-recently-used across all peers) aged separately, so the book could
offer a route whose proof the policy table had forgotten. Every eviction
rule that read policy state was defeated by pruning: a working route
whose state was pruned counted as never-worked after one blip; pruned
failure counts made a dead full book refuse a moved peer's new address;
a success later contradicted by an identity mismatch counted as proof
once the quarantine lapsed; and the dial ranking and the eviction
disagreed on which route "works". p2p-network-dev put three options to
architect-cto (retraction by the discovery provider; the book owns its
aging; revert to quarantine-only eviction) and recommended the second;
architect-cto decided it (message 01a0e64f-d87e-7678-b447-20bb61ddd4f0).

**What changed.** §Address-scoped failure gains "The book remembers the
proof": the address-scoped state (last success, consecutive failures
since it, identity-mismatch quarantine) is the book entry's, one owner,
bounded by the book — `max_addresses_per_peer` over classified peers
plus the retired-peer bound — and never pruned or evicted apart from it;
non-book addresses keep their state in the policy table; an address
entering the book takes its state with it, and an entry leaving the book
(eviction, permanent-failure removal, a retired peer) returns its state
to the policy table, which keeps a live quarantine until it lapses and a
failure-only record only as it keeps any non-book address, so a
re-learned address always re-enters under a live quarantine and leaving
the book launders none. One *recently good* predicate — a success and a
consecutive-failure count of zero, the count a success resets; no
last-failure stamp — serves the ranking and the eviction. The ranking
orders what to try (recently good, then fewest failures, then address).
The eviction is its own class order: a quarantined entry; a
never-successful entry that has failed, most failures first; a
successful entry that has failed since, oldest success first, never the
most recently proven route; it never takes an entry with no failure,
untried included. An identity mismatch erases the address's last success
as well as quarantining it. The peer-level backoff's "eligible known-good
address" keeps its meaning (a recorded success a mismatch has not erased,
not quarantined). No stamp is a provenance field. The §Implementation
implications sentence "a bounded dialable address book containing
provenance, last authenticated success, address-scoped failure/backoff,
and identity-mismatch quarantine" now reads that the entries carry the
state and that the door an address entered by is the operator set
(ADR-0052 rule 9, 2026-09-25), which the sentence predated; the mismatch
paragraph's "record the provenance/source that supplied it" — nothing
recorded a source and rule 9 forbids the tag — reads "record no source
tag".

**Corrections from the supplier review before hand-off.** The first
wording called the eviction "the ranking's reverse" and listed an order
that was not one (a never-successful entry with one failure ranks ahead
of a proven route with three failures since, yet was evicted first), and
let an untried entry be evicted where the code at d4918270 refuses to
displace any entry with no failure or the most recently proven route
(`the_address_book_is_bounded_per_peer`,
`a_full_book_never_gives_up_the_peers_last_proven_route`); it left
unstated where an entry's state goes on leaving the book, and a later
draft claimed the failure count survives a re-learn, which the policy
table's idle prune and bound make false by design (a failure-only record
that could never be pruned is the exhaustion path its
`is_punitive_at` comment names); it listed a last-failure stamp with no
reader; and it replaced the source-tag clause with a per-class mismatch
count nothing implements; and a third pass found the "always a live quarantine" claim unconditional where the policy table can refuse a returned quarantine (its bound reached, every record punitive — reachable by repeated third-party assertions answered by another peer), so the text now refuses the eviction or the retirement instead of dropping the quarantine; and the implementing review (#137, F6 at 736de328) noted the permanent-failure removals take the same hand-over, so the sentence names all three ways out. All corrected in the text above.

**Rejected.** Discovery retraction as the aging mechanism: withdrawal is
discovery's knowledge, the book learns from dials, and Identify has no
withdrawal — it may be added later as a feeder courtesy. Reverting to
quarantine-only eviction: the moved peer is Stage 12's first real case.

**Implementation state.** Decided for #137's head d4918270 (held by the
owner for this decision); p2p-network-dev implements it on that branch,
citing this amendment.

### Amendment 2026-10-01 — A new address makes the retry due: an untried address learned for a peer in dial-failure backoff is dialled at once, once per settled attempt

p2p-network-dev measured (GZCoord 01a0f6b4) a composed node restarted
with a peer cache holding peer B's old address and a static-bootstrap
entry for B's new one: the cached candidate is learned and reconnected
first, its dial refused, a peer retry scheduled at `RETRY_BASE_MS`
(30 s); the static provider's fresh address is learned a round later
and the gate refuses the reconnect because the peer is in backoff. Five
runs of six on main b87be549 failed to connect within 10 s; with cache
and static agreeing, six of six connected. §Address-scoped failure
already said a never-successful address failure does not advance the
PeerId into punitive backoff while another known-good address exists;
the retry schedule is per peer, so a good address arriving after the
failure inherited the stale one's wait. CLAUDE.md §5's "a bad/mismatched
address must not unnecessarily suppress a known-good route to a trusted
PeerId" names the same intent.

Ruled (01a0f6b5): learning an address the book admits that is untried
for the peer makes the peer's dial-failure retry due at once, for one
dial of that address; the address earns its own state from that dial
and the peer's retry entry keeps its attempt count, the next failure
setting the due time anew. Implementing it (b9e3888e) found the second site
the ruling had not named: the dial the scheduler then made was refused
`DialDenial::PeerBackoff`, because admission refuses every dial to a
peer in backoff and the failure record sets that backoff whenever no
known-good alternative exists at the failure — which a late-arriving
address cannot be. So admission's peer-backoff refusal is lifted for a
non-empty address with no record, once, its dial settling the record
that binds it after; the supplier review of this note then found that
the first form of the lift (b9e3888e) also lifted it for the pending-
dial hook's placeholder — an empty address that never earns a record —
so every behaviour-originated dial to a backed-off peer was admitted,
against §Decision item 3; acd8c7a6 binds the placeholder as before
(`a_placeholder_address_is_bound_by_the_peers_backoff`). Both halves are pinned
(`connection_manager.rs::a_newly_learned_address_makes_the_retry_due_once`,
`connection_policy.rs::an_untried_address_is_not_held_by_a_backoff_it_did_not_earn`,
each failing with its half mutated away); p2p-network-dev reports the stale-
cache + fresh-static restart connecting six in six in about 17 ms (the
same GZCoord thread; no end-to-end test in the tree reproduces it). Once per newly learned address, never for a
re-learned one with a live record; only the peer's dial-failure backoff
is lifted, for an untried address, and nothing else changes; the abuse
angle — an authorized peer advertising fresh
addresses to cut its own backoff — is bounded by the book's admission
(ADR-0052, `max_addresses_per_peer`, the untried-route protection), one
dial per admitted new address per settled attempt, each failure earning that address its
state and the peer another attempt. The composition round's "a peer in
backoff is the gate's to refuse" is narrowed, not removed: the gate
still refuses a peer in backoff, except once for a non-empty address
with no record — the retry schedule and the gate's peer backoff are two
tables, and both had to yield. The limit of "once" was then measured (01a0f6d9): a peer in dial-failure backoff, two Manual admissions of one untried address before either settles, both admitted — the snapshot the gate decides against carries no in-flight marker. Ruled as a recorded limit, not a defect: the pending-dial ceiling bounds what is in flight (`connection_manager.rs::the_pending_ceiling_holds_against_concurrent_admissions`), dials already admitted are not recalled, the first failure to settle writes the record that refuses later admissions (a success clears the backoff), and marking the attempt in the snapshot is a design change not ruled; the body reads "once per settled attempt". The code and its tests are p2p-network-dev's, in
the composition-hardening pull request this note lands on.

### Amendment 2026-10-09 — A network addition lifts the per-peer backoff, once per lift floor

**Trigger.** Stage 17 step 5 (plan §20), the network-change binding
p2p-network-dev-01 built on `feat/network-change-binding`: on an
addition to the host's known IP set, the relay reservation ladder was
ruled due (CONNECTIVITY.md §14, A 2026-10-09), and the measurement
showed it was not enough — the offline failure had also put the relay
PEER in the dial gate's backoff, so the ask failed at once. The lift of
that backoff for every classified peer (90af0da5) is the same per-peer
backoff this ADR's A 2026-10-01 lifts for a new peer address, with an
abuse bound recorded there; the blind review (F2) found this lift
recorded only in CONNECTIVITY.md, with no bound and no record here.

**Decision.** A new address of this host lifts the per-peer dial
backoff as a new address of the peer does: for every classified peer
(unauthorized peers lifted nothing), with the scheduler's retry made due
for data-plane peers only and the relay ladder made due, attempt counts
kept. The bound is a LIFT FLOOR: one lift per peer per cadence first
step (30 s for a peer's schedule, `retry_min` for the relay ladder),
measured from that peer's previous lift, so repeated additions — IPv6
prefixes announced by a LAN router, a flapping VPN or Wi-Fi, the two
detector sources lagging — redial a held-off peer at most once per
floor, under the existing dial ceilings. The first fill of an empty
known set runs the lift though it is reported as no change (R1: an
Android Service started offline fails its first dials before any
address exists; coming online would otherwise lift nothing).

**Alternatives rejected.** No floor, the rate following the addition
rate: bounded globally by the scheduler's and discovery's take, but
every held-off peer redialled per addition, which a noisy LAN produces
for free. Narrowing the lift to relay candidates: the staleness of the
host's failures is not a property of the peer's role.

**Consequences.** Code: p2p-network-dev-01's, on the same branch — the
floor beside the lift (a test: two additions inside the floor lift
once; one after it lifts again) and the first-fill lift (a subject
starting with no non-loopback address, failing a relay ask, then a view
with one address: the ask is due; the control without a view waits).
