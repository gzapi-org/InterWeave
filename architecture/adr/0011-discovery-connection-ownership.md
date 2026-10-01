# Discovery and connection management are separate

**Status:** Accepted

## Context

Discovery mechanisms produce information; connection policy depends on trust, topology, limits, retry/backoff state, and protocol needs. Combining them makes providers control the Swarm and prevents composition. The v1 static trust model also needs a clear ruling on whether candidate discovery alone is sufficient to create a connection.

A libp2p-specific complication is that a `NetworkBehaviour` can request a dial from the Swarm while driving its own protocol. Kademlia iterative queries are one such case. Therefore "the provider does not call the ordinary dial scheduler" is not sufficient to guarantee that global connection limits and punitive per-peer backoff are respected.

## Decision

DiscoveryManager owns candidate knowledge. **ConnectionManager owns connection policy**, including trust admission, reconnect policy, per-peer backoff, retention, and connection/dial limits. Libp2p-specific execution lives in the backend; normalized connection state is reported upward.

For v1 ordinary data-plane operation, connection policy is trust-gated. Mandatory Phase 9 adds one explicit, narrower connection class from ADR-0036:

- a data-plane candidate PeerId is not intentionally dialed unless the active `PeerTrustPolicy` authorizes that peer for data-plane connectivity;
- a PeerId in `transport.connectivity.infrastructure.allowed_peers` may be dialed/retained **only** for the protocol-scoped Identify/AutoNAT/relay control purposes defined by ADR-0036;
- an authenticated PeerId in neither authorization set is closed/rejected;
- infrastructure-only connectivity never grants direct, GossipSub, endpoint-directory, or Kademlia data-plane participation;
- trust/infrastructure revocation of a connected peer triggers appropriate protocol eviction/disconnect;
- discovery can still observe bounded candidate metadata for unauthorized peers without authorizing a connection.

### Swarm-wide dial admission

ConnectionManager policy applies to **every outbound Swarm dial**, not only calls initiated by the ordinary candidate dial scheduler. `transport-libp2p` therefore includes an internal, synchronous **DialAdmissionGate** (or equivalent root-behaviour hook) fed from ConnectionManager state. Dial failure accounting is split into **peer-scoped policy/backoff state** and **address-scoped reachability/authentication state** so a poisoned address cannot unnecessarily suppress a trusted peer's known-good route. Before a Swarm dial is allowed, the gate enforces at least:

1. destination connection class (`DataPlaneTrusted`, `ConnectivityInfrastructureOnly`, or unauthorized) and the requested dial origin/purpose;
2. current `PeerTrustPolicy` / connectivity-infrastructure authorization when the PeerId is known;
3. per-peer punitive/retry backoff;
4. global pending-dial and connection limits;
5. profile shutdown/drain state;
6. address/path policy checks available at that boundary.

A protocol behaviour such as Kademlia may *request* a dial as part of an iterative query, but it does not own the decision to permit that connection. A denied behaviour-originated dial is observable as policy/backoff/limit denial and must not silently reset ConnectionManager retry state.

### Discovery never writes the address book

A discovery provider yields candidates; it does not write the Swarm's
address book (A 2026-09-20). A provider's underlying transport behaviour
that emits address facts to the Swarm — `libp2p-mdns 0.49` pushes
`ToSwarm::NewExternalAddrOfPeer { peer_id, address }` for every pair it
hears on the multicast domain, with no trust check of any kind — is
wrapped, and that emission is swallowed at the wrapper, the way the relay
client's own reservation confirmation is. The only route from a
discovered pair to a dialable address is the provider's normalization,
bounds and dedup into `DiscoveryManager`, and from there through the
ConnectionManager admission above — into the ConnectionManager's own
bounded dialable address book (§Implementation implications), which
is where a candidate is MEANT to arrive; the Swarm's book is the one
it never touches. `libp2p-request-response 0.30.0` DOES consume
`FromSwarm::NewExternalAddrOfPeer`, into its `PeerAddresses` cache
(corrected A 2026-09-25: the record had said no enabled behaviour did),
which is why the swallow is enforcement today and not a precaution.
And the emission was one of TWO doors: `libp2p-mdns 0.49.0` also
answers the Swarm's pending-dial hook with every address multicast
named for the dialled peer, which the Swarm appends to any dial that
extends through the behaviours — Kademlia's, the relay client's,
request-response's. The wrapper forwarded that hook until PR #111
commit f85dd27; it now answers with nothing, and the multicast
conformance test binds both doors. A
discovery-supplied address is inside ADR-0052's boundary — it is an
address this runtime dials because a peer supplied it — so the
provider's instance of that record's rules 3 and 4 is stated in its
provider document before the dial hook is written (ADR-0052 rule 8;
for mDNS, `providers/mdns.md`), and the boundary's floor (no DNS
name, no circuit component, no link-local) stands. Binds every
provider, present and next.

### Address-scoped failure and poisoned-address resistance

ConnectionManager tracks failure/backoff for each normalized dial address separately from peer-level punitive state. Recently authenticated-successful addresses are preferred over never-successful addresses. A never-successful address failure does not advance the whole PeerId into punitive backoff while another eligible known-good address exists. If Noise authenticates a different PeerId than the dial target, that is an **address identity mismatch**: close the connection, quarantine that address for 30 minutes by default, record no source tag (the only door fact is whether the address is in the operator set — ADR-0052 rule 9; A 2026-09-28), and do not penalize the expected trusted PeerId's peer-level backoff. Peer-level backoff advances only for failures that remain meaningfully peer-scoped after eligible address alternatives are considered.

Address failure state remains bounded by the address-book limits. A successful authenticated connection resets the successful address's failure state; it does not automatically rehabilitate unrelated quarantined addresses.

**A new address makes the retry due (A 2026-10-01).** The retry schedule for dial failures is kept per peer, so a known-good address learned a round after a stale one failed would inherit the wait the stale one earned — the case the paragraph above foresaw, with the good address arriving late. Two sites together make an untried address dialable at once. Learning an address the book admits that is UNTRIED for that peer — no record of its own — makes the peer's dial-failure retry due now, if unclaimed; and admission's peer-backoff refusal is lifted only for a NON-EMPTY address with no record, so that address is admitted under the peer's dial-failure backoff once per settled attempt — its dial settles a record, after which the backoff binds it; a behaviour-originated dial's placeholder (an empty address, which never earns a record) stays bound as before. Either half alone is not enough: a due retry whose dial admission refuses, or an admitted address no scheduler asks for, leaves the fresh route waiting. The dial earns the address its own state; the peer's retry entry keeps its attempt count, so the next failure backs off from where the failed addresses left it, and sets the due time anew. Once per newly learned address, never for a re-learned one whose failure or quarantine record the book or the policy table still holds (an address that came back untried because its failure-only record was pruned counts as new). What is lifted is exactly the peer's dial-failure backoff of the paragraph above, for an untried address only; no other state changes, by construction: that table's backoff is set only by the dial-failure record (a success, a prune or an eviction only removes it); an identity mismatch quarantines the address, and trust is the class. The abuse this admits is bounded twice: the retry half by the book's admission (ADR-0052's boundary, `max_addresses_per_peer`, the untried-route protection), the admission half by the policy table's record per address and the root funnel every dial passes — one immediate dial per new address per settled attempt, each failure earning that address its state and the peer another attempt. "Once" is per settled attempt, not per address in flight: the gate decides against a published snapshot that carries no per-address in-flight marker, so two dials of one untried address admitted before either settles are both admitted (two command dials, as p2p-network-dev measured); the global pending-dial limit (item 4) bounds what is in flight, dials already admitted are not recalled, and the first failure to settle writes the record that refuses later admissions under the backoff (a success clears the backoff instead). Marking the attempt in the snapshot would make the lift strictly once; the bound already holds, so that change is not ruled.

**The book remembers the proof (A 2026-09-28).** The address-scoped state above — last authenticated success, consecutive failures since the last success, identity-mismatch quarantine — is a property of the address's BOOK ENTRY, with one owner. The book's bound (`max_addresses_per_peer` over classified peers, plus the retired-peer bound the book keeps for peers the profile no longer classifies) is the memory bound for that state: an idle-time prune or a global least-recently-used eviction of the policy tables never forgets a book entry's proof, because a route the book still offers to the dialer must carry what the dialer knows about it. Addresses that are not book entries (a command's one-off dial) keep their state in the policy table as before; an address that enters the book takes its state with it, and an entry that leaves the book — evicted, removed on permanent failure, or dropped with a retired peer — returns its state to the policy table, which keeps a live quarantine until it lapses (its eviction never clears one) and keeps a failure-only record only as it keeps any non-book address, until the idle prune or its own bound; a re-learned address re-enters carrying whatever the table still holds — always a live quarantine, so leaving the book launders no quarantine. When the table cannot take a live quarantine (its bound reached and every record punitive), the entry does not leave: the eviction is refused and the book refuses the newcomer, a permanent-failure removal leaves the route in the book undialled until its quarantine lapses, and a retirement pass leaves the quarantined entry in place for a later pass — every way out goes through the one hand-over, and the book never drops a live quarantine it cannot hand over. One predicate serves both the dial ranking and the eviction: an entry is *recently good* when it has a success and its consecutive-failure count is zero (the count a success resets); there is no separate last-failure stamp. The RANKING orders what to try: recently-good entries first, then by fewest consecutive failures, then by address. The EVICTION on a full book is not the ranking's mirror — it protects what has not been disproven — and takes, in order: a quarantined entry; a never-successful entry that has failed, most failures first; a successful entry that has failed since, oldest success first, never the peer's most recently proven route. It NEVER takes an entry with no failure — recently good or never yet tried — nor the most recently proven route: a book of working and untried routes refuses a newcomer, so a stream of third-party assertions through the discovery door cannot churn out a peer's untried Identify or configured routes; a peer that moved is evictable because its old route has failed since it last worked. An address identity mismatch erases the address's proof (its last success) as well as quarantining it: a route that authenticated as another peer is not a proven route for the expected one, and the quarantine's lapse restores dialability, never proof. The peer-level backoff's "another eligible known-good address exists" (above) keeps its meaning — an address with a recorded success (one a mismatch has not erased) that is not quarantined — and is not this predicate. No stamp is a provenance field (ADR-0052 rule 9): stamps record outcomes of this node's dials, which no peer chooses.

This preserves the architectural invariant while acknowledging libp2p execution reality:

> ConnectionManager owns connection policy; the Swarm/backend executes dials. Protocol behaviours may generate dial requests only through the same Swarm-wide admission policy.

ADR-0036 is the first explicit protocol-scoped exception: connectivity-infrastructure-only peers may carry Identify/AutoNAT/Circuit-Relay control traffic but remain excluded from the application data plane. **ADR-0009's Kademlia integration does not use this exception** and still requires data-plane trust for routing peers.

## Alternatives considered

Providers dial directly; DiscoveryManager owns Swarm; Transport core implements multiaddress dialing itself; connect to every discovered peer but gate only local message delivery; exempt Kademlia-generated dials from ConnectionManager backoff; attempt to force all protocol queries through the ordinary explicit-dial API.

## Consequences

There is an explicit handoff and policy-snapshot synchronization cost, but failure ownership is clear and testable. Small/asymmetric trust sets can constrain overlay connectivity; that is an accepted consequence of the v1 deny-by-default model.

Kademlia query progress may cause dial requests that were not scheduled by the ordinary candidate dial loop. Those attempts still consume global connection resources and obey backoff through `DialAdmissionGate`. Diagnostics therefore distinguish **dial origin** (`connection-manager`, `kademlia-query`, `relay-reservation`, `relay-circuit`, `autonat-probe`, `dcutr-hole-punch`) where the backend can attribute it.

## Security implications

Untrusted discovery cannot force successful connections merely by being discovered or returned in a Kademlia response. Swarm-wide admission applies trust and resource policy before a behaviour-originated connection is established, reducing amplification, connection storms, and unintended GossipSub exposure. Address-scoped mismatch quarantine prevents an attacker who can inject a bogus address for a trusted PeerId from turning that one address failure into peer-wide punitive backoff while a known-good route remains available.

## Operational implications

Backoff and global limits are consistent across explicit candidate dials and protocol-generated dials. Provider outages do not tear down good trusted connections. Trust reload may intentionally disconnect peers and change mesh/routing topology; this is observable via `TrustPolicyChanged` and peer-disconnect diagnostics.

SPIKE-003 must measure Kademlia-originated dial attempts under this gate. Mandatory `SPIKE-004` must do the same for AutoNAT/relay/DCUtR behaviour-originated dials and prove that infrastructure-only authorization cannot leak into GossipSub/direct/endpoint/Kademlia participation.

## Implementation implications

Backend consumes normalized candidate updates and maintains a bounded dialable address book whose entries carry last authenticated success, address-scoped failure/backoff, and identity-mismatch quarantine (§Address-scoped failure, "the book remembers the proof"); which door an address entered by is the operator set beside the book, never a field on the entry (ADR-0052 rule 9) (A 2026-09-28). ConnectionManager publishes an atomically readable policy snapshot to the Swarm task / `DialAdmissionGate`; the gate must not block on async policy calls while the Swarm is being polled. Policy revision changes invalidate stale authorization/backoff snapshots promptly.

Before retaining an inbound data-plane connection, ConnectionManager applies the same current authorization policy. Successful observations report back for cache hints. Unauthorized candidates remain diagnostics/discovery state, not active transport peers.

The Kademlia driver remains Swarm-owned. Its iterative queries may produce `ToSwarm::Dial` requests, but those requests are subject to `DialAdmissionGate`; the provider itself still does not dial.

### Identify's advertised addresses enter the book through ADR-0052's boundary

An Identify `listen_addr` is peer-supplied in ADR-0052 rule 1's own words — the peer chose it, this node dials it — and the address book is what the retry scheduler dials from unprompted. It is therefore an instance of ADR-0052 rule 8 (A 2026-09-20), which makes **every path by which a peer-supplied address enters the book or a dial** an instance rather than only a path that originates a dial.

This path went without a hook longer than the others because nothing about it looked like a dial and because the build could not dial the interesting half anyway: a `/dns4/` address a peer advertised failed `MultiaddrNotSupported`, classified structural, and was evicted. The refusal read as a rule while it was an accident of the Swarm being composed `with_tcp` alone. Building the DNS transport removed the accident and left the rule to be written.

**The instance.** The floor, rule 2 included: a literal `/ip4/` or `/ip6/` address, none of the special-use ranges — and **no DNS name**, because to a peer the resolver is an oracle. A `/p2p-circuit` address the peer advertises about itself IS admitted (A 2026-09-25) when its transport prefix, through the relay's `/p2p/<relay>` component, is such a literal or an operator address (ADR-0052 rule 9): a NATed peer's advertised circuit is its only route, and refusing every circuit as `Relayed` — the dial-back's and the punch's clause, where a relayed address answers the wrong question — left `DialPeer`'s circuit fallback nothing to dial for a peer learned through Identify alone. The prefix ends in the relay's `/p2p/<relay>` and what follows `/p2p-circuit` is nothing or the advertising peer itself; a foreign destination or a nested circuit is refused under its own shape class. The boundary judges the relay's ADDRESS; whether this node may dial through that relay is the gate's question at dial time (`RelayCircuit` origin, ADR-0036's class), and a circuit on a node without the relay transport is refused as structural. The entry counts against the same per-peer cap as any other. A name in the operator's own configuration is a different question and still resolves at dial; that is what the DNS transport exists for. Rule 3 admits a private address (RFC 1918, IPv6 ULA) where this node holds a non-loopback listener in a private range of the same family: a LAN peer legitimately advertises its private address, and a node with no such listener has no LAN interface to reach it on. There is **no rule-4 source-equality clause** — a peer behind NAT legitimately advertises a listen address that differs from the address its connection was observed from.

**Where it is enforced.** At the LEARN site, before the address becomes a book entry, because Identify originates no dial and so there is no crate dial to deny and reissue (rule 5). A refused address never becomes an entry at all, so a later relaxation of the retry or admission path cannot launder one. The predicate is a sibling in the same module as the probe, punch and discovery predicates, under rule 6's subset test; it is a distinct name rather than a reuse because "may a peer's own listen address be believed" and "may a multicast announcement be believed" are two questions that happen to share an answer today, and one name would make the next divergence silent. A refusal is counted by class and the address itself is never logged.

**What this does not change.** An advertised address was already advisory rather than authorization, bounded per peer, and remembered only for a classified peer; every dial from the book still passes `DialAdmissionGate`. The boundary narrows what may be remembered — it grants nothing.

**What it is, and is not (A 2026-09-25).** This is a STORE hook in ADR-0052 rule 8's terms: it keeps the book clean, and it is not the dial's enforcement. Identify also fed a second store nobody owned — `libp2p-identify`'s own address cache, one hundred entries by default, returned from the crate's pending hook to any dial that extends its addresses through the behaviours — and that cache is disabled (`with_cache_size(0)`): the runtime's book is the only address store the runtime WRITES from Identify. The other address stores in the process are named here with their doors, so nobody reads "only" as "sole" (A 2026-09-25): Kademlia's routing table, hooked at `admits_offer`, and its in-query addresses, pruned at ADR-0052's root funnel; request-response's `PeerAddresses`, fed only by `NewExternalAddrOfPeer`, which the wrappers swallow; and the mDNS crate's own record store, which reaches no dial and is recorded in `DISCOVERY-CONFORMANCE.md` (Decision 2026-09-25). What a behaviour-extended dial then carries is enforced once at ADR-0052 rule 5's root funnel, the class counterpart of this record's root-level dial gate, on the same pull request.

## Revisit conditions

Revisit if a backend cannot enforce a root-level outbound dial gate, if a future protocol needs an explicit non-data-plane connection class, or if empirical evidence shows behaviour-generated dial attribution/backoff cannot be enforced without a different Swarm composition. Do not weaken discovery-versus-connection ownership implicitly.

## Amendments

Full notes: [`history/0011-amendments.md`](./history/0011-amendments.md).

| Date | Amendment | Effect |
|---|---|---|
| 2026-09-20 | Discovery never writes the address book | §Decision gains the rule: a discovery provider yields candidates and never writes the Swarm's address book; a transport behaviour's address emission (`libp2p-mdns 0.49` `NewExternalAddrOfPeer`) is swallowed at the wrapper; the only path to a dialable address is normalization → `DiscoveryManager` → ConnectionManager admission. Binds every provider. |
| 2026-09-20 | Identify's advertised addresses enter the book through ADR-0052's boundary | §Implementation implications gains the subsection: an Identify `listen_addr` is peer-supplied and is an instance of ADR-0052 rule 8, hooked at the learn site with the floor, rule 3 beside a same-family private listener and no rule-4 clause; a refused address never becomes an entry. Landed with the DNS transport on PR #111; the record was owed and is written 2026-09-25. |
| 2026-09-25 | The Identify instance admits a peer's own circuit address | §Identify's advertised addresses: an advertised `/p2p-circuit` whose transport prefix through the relay's `/p2p` component meets ADR-0052's floor or is an operator address enters the book, counted against the per-peer cap; the relay's admission stays the gate's at dial time. Before this a NATed peer's only route never entered the book (the DNS review of PR #111). |
| 2026-09-25 | Two doors, several stores | §Decision: `libp2p-request-response 0.30.0` consumes `NewExternalAddrOfPeer` (the record had said no enabled behaviour did), and a wrapped behaviour has a second door — its answer to the Swarm's pending-dial hook, which `MdnsScope` now answers with nothing (PR #111 f85dd27); §Identify names the other address stores in the process with their doors, so "the only address store" reads as the only one the runtime writes from Identify. |
| 2026-09-25 | The Identify hook is a store hook; the crate's address cache is disabled | The subsection says what it is not — the dial's enforcement, which is ADR-0052 rule 5's root funnel — and records that `libp2p-identify`'s own address cache, a second store nobody owned, is disabled so the book is the only address store. |
| 2026-09-28 | The book remembers the proof: an address's dial state is its book entry's, bounded by the book, never pruned apart | §Address-scoped failure gains the paragraph: the address-scoped state is the book entry's, one owner, bounded by the book and never pruned or LRU-evicted apart from it; one *recently good* predicate serves ranking and eviction, a recently-good entry is never evicted, an identity mismatch erases the address's proof. An entry leaving the book returns its state to the policy table, which keeps a live quarantine (laundering none); the eviction is stated as its own class order, never taking an entry with no failure or the most recently proven route. §Implementation implications: "containing provenance" corrected to ADR-0052 rule 9's operator set; the mismatch paragraph's "record the provenance/source" becomes "record no source tag". |
| 2026-10-01 | A new address makes the retry due: an untried address learned for a peer in dial-failure backoff is dialled at once, once per settled attempt | §Address-scoped failure: two sites — the per-peer retry schedule yields to a newly learned, untried address the book admits, and admission's peer-backoff refusal is lifted only for a non-empty address with no record (a behaviour dial's placeholder stays bound) — one immediate dial, the address earning its own state, the peer's entry keeping its attempts; never for a re-learned address with a live record; only the peer's dial-failure backoff is lifted, for an untried address; bounded by the book's admission, the policy table's record and the pending-dial ceiling. Measured by p2p-network-dev: a restart with a stale cached address and a fresh static one lost up to 30 s in 5 runs of 6. |
