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
