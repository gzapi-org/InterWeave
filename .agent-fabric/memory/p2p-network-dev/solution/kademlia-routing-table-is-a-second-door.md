---
role: "p2p-network-dev"
class: solution
topic: "kademlia-routing-table-is-a-second-door"
description: "Identify listen_addrs reach TWO dialled stores — the address book and Kademlia's routing table — so a peer-address boundary on the book alone is half-closed; admits_offer is the routing table's funnel"
tier: 2
knowledge_scope: full
distilled_at: "2026-09-26"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - a93203683c4980f4
---

## Identify listen_addrs reach TWO dialled stores — the address book and Kademlia's routing table — so a peer-address boundary on the book alone is half-closed; admits_offer is the routing table's funnel

`classify_swarm_event` says it in as many words: Identify "feeds three
consumers — this pipeline (F3), the address book, and the consumer's
`Identified` event". Two of those are stores that get DIALLED.

On PR #111 the ADR-0052 Identify instance was first built on the book
alone (`learn_advertised`, `a9d4779`). Reading the paths architect-cto
named "to be read" found the same `listen_addrs` still reaching
`add_address` through `pending_offers` — and Kademlia dials
routing-table addresses. A peer-supplied `/dns4/` name still reached a
resolver and a socket with the book's door shut.

`pending_offers` has exactly two writers, and one funnel now covers
both (`admits_offer`, `66e5663`):
- `observe_identify` — the peer's own `listen_addrs`;
- `KademliaCommand::OfferRoutingPeer` — built by the Kademlia provider
  from a `PeerHint::CandidateHint`: a query result or a `PeerCache`
  hint on re-entry.

The stash's 64-address cap and `MAX_PENDING_OFFERS` are BOUNDS, never a
class boundary. The listener set rule 3 needs is refreshed before each
dispatch (`set_own_listeners`), at `commands.rs`'s Kademlia arm and
`mod.rs`'s `handle_kademlia` call.

The book's writers, all accounted for: the Identify arm (filtered), a
connected route from the ticket (not peer-supplied), the operator's
`AddAddress` (outside the boundary), ticket-derived re-learning.

**Why:** a half-closed boundary is worse than an open, named one — the
tree looks protected.

**How to apply:** before claiming a peer-address boundary is closed,
list every store the input reaches AND every store that is dialled,
not only the one the finding named. See [[dns-wrap-reshapes-every-dial-error]]
for the change that exposed this.

*References: dns-wrap-reshapes-every-dial-error*

*Observed 2026-09-25 (p2p-network-dev)*
