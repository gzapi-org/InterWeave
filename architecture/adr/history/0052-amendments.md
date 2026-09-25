# ADR-0052 — amendment history

### Amendment 2026-09-20 — Peer-supplied names and the paths that enter the book

Raised by p2p-network-dev on PR #111, which builds the DNS transport
(Stage 11's fifth obligation): before it, a `/dns4` address a peer
asserted in Identify's `listen_addrs` was learned into the address book
and failed `MultiaddrNotSupported` on dial — classified structural and
dropped, by accident of a TCP-only Swarm. After it the same address is
resolved and dialled, so a classified peer could make this node query a
name of its choosing and connect to whatever it resolved to. The
record's revisit clause said to reopen it when a name the runtime must
resolve arrived; it was reopened the same day.

Prior wording, rule 2's first clause: "the candidate is a literal IP
multiaddr — no DNS name (the resolver is an oracle)". Prior wording,
rule 8: "A new peer-supplied dial reads this record first. A protocol
that would dial an address a peer named … states its instance of rules
3 and 4 in its transport document before its hook is written".

What changed and why. Rule 1 was read as written — every address dialled
because a peer supplied it — and the Identify path was found inside it
with no hook: `dialing.rs`'s Identify arm learns every `listen_addr`
through `learn_route` into `ConnectionManager::learn_address`, which
checks only that the peer is classified and caps the count. That gap
predates the DNS transport (a loopback or private listen_addr from a
classified peer was learnable and dialable before #111); the transport
widened it from addresses to names. Rule 2's refusal of a name became a
resolver policy rather than a bare refusal, because the design has always
resolved names — ADR-0010's configured bootstrap entries — and what
distinguishes those from a peer's name is who supplied them, not whether
a transport can resolve them: the operator's names resolve, a peer's are
refused at the boundary, and the transport stays. Rule 8 widened from
"a new dial" to "every path that enters the book or a dial", naming the
learn site as the hook where a path originates no dial (mDNS's shape,
built the same day), so that the next path is an instance by rule
rather than by someone noticing. Kademlia routing records and PeerCache
re-entry were named as paths to be read and stated in the change that
reads them, not asserted covered.

The Identify hook lands on PR #111 beside the transport — rule and hook
together, the #102 precedent — as a sibling predicate under rule 6's
subset test, with the instance stated in ADR-0011 §Implementation
implications. The alternative of taking the DNS transport back out
until a resolver policy existed was declined: the policy is one
sentence and the hook one predicate call, and holding a built transport
hostage to them would have re-created the accident (names unresolvable
because of what was built, not because of what was decided).

### Amendment 2026-09-25 — Dials are enforced once, at the root of the outbound path

Raised by p2p-network-dev on PR #111 after the blind re-review of
`6a40a37..8a1d98a` (comment 5827859142) found five paths by which a
peer-supplied address still reached a socket after the two learn
sites of 2026-09-20 had been hooked — libp2p-identify's own address
cache (default 100 entries, returned from its pending hook to any dial
that extends its addresses through the behaviours: Kademlia's walk,
the relay client's reservation dial), Kademlia's in-query FIND_NODE
addresses, the AutoNAT learned-server list, the relay reservation
list, and query-result candidates leaving the driver unfiltered. It
was the third round on one invariant; p2p-network-dev stopped hooking
sites and asked where the rule is enforced for a dial a behaviour
starts itself.

Prior wording, rule 8's opening: "Every path by which a peer-supplied
address enters the book or a dial is an instance, and reads this
record first … Where the path originates no dial, the hook is the
learn site". Prior wording, rule 2: "the local `LearnAddress`
command". Prior wording, rule 5, unchanged in substance: "the hook can
add addresses but not remove one — so the filter runs there by
deny-and-reissue".

What changed and why. The read-back of the pinned `libp2p-swarm`
0.48.0 (`Swarm::dial`) settled the locus: the Swarm calls the ROOT
behaviour's pending-outbound hook once and extends the dial from what
that one call returns, so a wrapper around the composed behaviour —
not a sibling field, which sees only the explicit list (`outbound_gate.rs`
F9) — is the one place every behaviour-contributed address passes
before a socket, and can prune it. Rule 5 gains that clause for
behaviour-extended dials, deny-and-reissue staying the shape for the
explicit list. Rule 8 is restated in two halves because the two hooks
PR #111 built are STORE hooks — they keep the book and the routing
stash clean, which matters because a stored address is later dialled,
offered or handed on — while the DIAL is enforced at the root and
nowhere else; a per-site dial predicate is a diagnostic refinement.
The instance list now says which two stores are hooked, which paths
are not yet enforced, and the two facts that bound the exposure:
Identify's crate-level cache is disabled, and no production composer
of the runtime exists before Stage 12 — the runtime's own `start`
builds Kademlia and the AutoNAT client whenever its config enables
them, so the paths are live in every test or spike that does; the
plan's §15 precondition holds Stage 12 off them until the funnel has
landed with its measurement. Rule 9 answers the provenance question
under all of it: a single funnel must admit the operator's `/dns4`
seed and refuse a peer's, and the routing door as first built
(66e5663) refused the operator's own static-bootstrap name because
`OfferRoutingPeer` carries peer and addresses only. Provenance is made
a property of the DOOR — one operator set, held as a shared handle,
consulted at every store door and at the root — rather than a field on
the carrier, because a field a peer's path could set is a field a
peer's path will set. Rule 2 names `AddAddress`, the command that
exists; the `LearnAddress` it named did not, and the plan's Stage 11
record is corrected the same day. The owner chose on 2026-09-25 to
hold PR #111 for the full closure, so the funnel, its measurement and
the operator set land there beside the store hooks, in
p2p-network-dev's order: cache disabled, measurement, wrapper,
operator set.
