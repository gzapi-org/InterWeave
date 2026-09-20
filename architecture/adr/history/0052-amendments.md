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
