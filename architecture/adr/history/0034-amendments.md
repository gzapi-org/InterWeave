# ADR-0034 — amendment history

### Amendment 2026-09-27 — The composition path refuses an implied Kademlia default until the release gate is decided

Stage 12's composition batch (p2p-network-dev) will compose a running
`TransportRuntime` from a profile, Kademlia included when its entry is
enabled, and lift profile-config's `DiscoveryProviderNotImplemented`
for mdns and kademlia. Lifting kademlia outright would make an entry
that omits `enabled` run Kademlia — item 2's default — in every
runtime the library composes, while rule 7 withholds shipping the
default on until the gate is taken — blocked, per the plan's §14
closing record and CLAUDE.md §1, on Stage 12's composition root and
SPIKE-004's server-mode evidence; Stage 11 closed with that decision
recorded as separate and not taken. Rule 7 therefore gains the guard at
the composition path, in two stages of one load: deserialisation still
applies item 2's default and records that `enabled` was implied
(profile-config's model holds a plain `enabled: bool` today, so the
omission must be kept for validation to see it); validation refuses an
implied `enabled` with an error of its own naming this gate; an entry
stating `enabled: true` composes Kademlia; mdns is lifted entirely.
Until the gate is decided the default decides no outcome — the refusal
is about acting on the implied form, and a loud refusal was preferred
to a silently unstarted provider or to lifting fully, which would take
rule 7's decision by the side door. The refusal's removal is one change
on the owner's word when the gate is decided. Ruled 2026-09-27 on
p2p-network-dev's question, for their Stage 12 batch 2.
