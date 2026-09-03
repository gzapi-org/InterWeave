# ADR-0036 — amendment history

### Amendment 2026-09-03 — Relay circuit toward an infrastructure-only destination

SPIKE-004 measured the shipped policy admitting a `DialOrigin::RelayCircuit`
dial toward a `ConnectivityInfrastructureOnly` peer, and recorded it as
divergence D2. Whether that was a defect turned out to depend on what this
ADR had decided, and the honest answer was that it had decided nothing.

The prior matrix carried one relay row:

> `| Circuit Relay v2 reservation/circuit control | yes when eligible | yes when eligible |`

and `transport/libp2p/CONNECTIVITY.md` §11 named exactly two origins as never
using the infrastructure-only authorization set — `direct-user-command` and
`kademlia-query` — with `relay-circuit` listed among the origins and excluded
from neither. Read together, an implementer could reasonably conclude that
today's admission was permitted.

**It was silence, not permission.** The row says *control*, and the matrix
already distinguishes control from destination one row down — "DCUtR with that
peer as application destination | **no**" — while closing with "infrastructure-
only connections are therefore control-plane connections, not data-plane
membership". The circuit-destination case simply had no row.

The amendment adds it, `| Relay v2 circuit with that peer as application
destination | yes | **no** |`, and states the principle the resulting pair of
relay rows turns on: who an exchange is *with* is a different question from who
it is *for*. DCUtR needs no second row — its only row is already the
destination one.
A reservation is an exchange with the infrastructure peer for the purpose it
was authorized for; a circuit terminating at that peer uses it as an
application destination, and a circuit carries the data plane by construction.

This is a refinement the decision already implied rather than a re-decision,
so it is an amendment and not a superseding ADR (ADR-0048): a reader who
followed the old text and refused the dial was right then and is right now,
and one who admitted it was reading a control row as a destination rule.

**A stale citation of the single relay row is now incomplete rather than
wrong.** Anything citing "ADR-0036's relay row" to justify dialling a circuit
toward an infrastructure-only peer should be re-read against both rows.

The code that prompted this is unchanged by the amendment and remains
divergent until Stage 11 step 2 fixes it: `DialOrigin::is_data_plane` omits
`RelayCircuit`, so `ConnectionPolicy::admit` treats such a dial as
control-plane. `RelayReservation` must stay non-data-plane — a reservation is
the reachability purpose itself — so the fix distinguishes the two origins
rather than moving both.
