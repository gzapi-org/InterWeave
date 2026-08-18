# kademlia-control-api

Small internal neutral command/event port between the Kademlia provider and the Swarm-owned driver.

**Current status:** Stage 1, active workspace member. Types and routing-view arithmetic only.

## Two absences are the contract

**No record command exists.** `KademliaCommand` has no `put_record`, `get_record`, or `start_providing` variant. Kademlia here is peer routing only (ADR-0009), and the rule is not "the driver must not call those" — there is nothing to call. Any later record use needs a new ADR, and would appear here as a new variant a reviewer cannot miss.

**No dial command exists.** Iterative queries do make the Swarm request dials, but those are behaviour-originated and pass the root `DialAdmissionGate` (ADR-0011). A `Dial` variant here would be a provider-owned dial, which is precisely what that gate exists to prevent.

A test matches every variant exhaustively, so adding either one fails to compile before review.

## Why there is no `kademlia` schema family

Deliberate, and recorded in `architecture/contracts/schemas/manifest.json`. ADR-0049 schematises **wire shapes** — documents crossing a boundary as JSON — and Kademlia has none: this port is in-process, its peer observations reach consumers as ordinary `discovery.candidate-peer` documents, its one external string (`/interweave/kad/1.0.0/<network-hash>`) is a derived value frozen in `fixtures/kademlia/`, and record mode is disabled so the DHT stores nothing. A family here would have to invent documents nothing transmits.

## Saturation is a resting state, not a failure

`effective_target = min(target, max, remote_trusted_population)`. The third term is the one that bites: a profile trusting two peers cannot reach a target of 64 however long it explores.

So a view is healthy when target-satisfied **or saturated** — three consecutive exploration rounds finding nothing new. Without that, a small trusted overlay would report degraded forever while behaving perfectly. Exploration also backs off exponentially from its configured base, capped at 15 minutes, so that overlay does not run a useless 60-second loop indefinitely.

Zero remote trusted peers is different again: not degraded but `Unavailable`, because there is nobody to route with and retrying cannot change that.

`RoutingView` holds **counts, not peers** — the provider needs to know whether the view is satisfied, and handing it the membership would invite routing decisions the driver owns.
