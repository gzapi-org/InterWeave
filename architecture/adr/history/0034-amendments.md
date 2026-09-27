# ADR-0034 — amendment history

### Amendment 2026-09-27 — The composition path refuses an implied Kademlia default until the release gate is decided

Stage 12's composition batch (p2p-network-dev) composes a running
`TransportRuntime` from a profile, Kademlia included when its entry is
enabled, and lifts profile-config's `DiscoveryProviderNotImplemented`
for mdns and kademlia. Lifting kademlia outright would make an entry
that omits `enabled` run Kademlia — item 2's default — in every
runtime the library composes, while rule 7 withholds shipping the
default on until Stage 12 composition and SPIKE-004's evidence are both
consumed and the owner takes the gate; Stage 11 closed with that
decision recorded as separate and not taken. Rule 7 therefore gains the
guard at the composition path: an entry that omits `enabled` is refused
at parse with an error of its own naming this gate; an entry stating
`enabled: true` composes Kademlia; mdns is lifted entirely. Item 2's
default stays the decision and profile-config keeps applying it at
parse — the refusal is about acting on the implied form, and a loud
refusal was preferred to a silently unstarted provider or to lifting
fully, which would take rule 7's decision by the side door. The
refusal's removal is one change on the owner's word when the gate is
decided. Built on p2p-network-dev's Stage 12 batch 2; ruled 2026-09-27
on their question.
