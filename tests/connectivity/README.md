<!-- SPDX-License-Identifier: Apache-2.0 -->
# `tests/connectivity`

**Current status:** Stage 11, active workspace member.

Root connection and dial admission, over real sockets. Test-only —
nothing depends on this package.

Opened for the Stage 5 exit gate and no longer only that: Stage 11 added
`advertised_protocol_set.rs`, which pins what a default profile offers on
the wire now that the libp2p feature list no longer withholds the
connectivity behaviours.

## Why these run against a real Swarm

The clauses under test are about what the *substrate* does with policy,
not about whether the policy state machine is correct on its own. That
distinction is not academic: `policy.admit(&request, class, 0)` — a
literal zero where the clock belongs — passed every unit test in
`interweave-transport-runtime`, because those tests supply the clock
themselves. Only something driving the real dial path could notice that
the substrate never supplied one, so every backoff window and every
quarantine was evaluated at the same instant for the life of the
process.

A mocked transport would have proved that the translation layer
compiles.

## What is proved here, and what is not

| Exit-gate clause | Where |
|---|---|
| root admission is the only authority for outbound Swarm dials | `GatedSwarm` — a compile-time property, see below |
| denied autonomous-behaviour dials cannot reset backoff | here, at the manager layer, for the reason the test states |
| pre-Noise work is bounded | `interweave-transport-runtime::preauth` |
| address poisoning cannot suppress a healthy trusted route | `connection_policy::address_poisoning_cannot_suppress_a_known_good_route` |

The first clause has no runtime test because it is not a runtime
property. `GatedSwarm` owns the `Swarm` privately and `dial` takes an
`AdmittedDial`, which cannot be constructed without a `DialTicket`,
which only `PolicySnapshot::admit` issues. A call site that skips
admission does not fail a test — it fails to compile.

**The behaviour-originated dial path was not yet closed when this
package opened**, and no Stage 5 test here should be read as claiming it
was. Stage 4's behaviour set was TCP, Noise, Yamux and Identify, none of
which dials, so there was nothing to gate; the hook is
`NetworkBehaviour::handle_pending_outbound_connection` and it had to
require the same ticket before Kademlia, AutoNAT, Relay or DCUtR was
enabled (CLAUDE.md §3).

**That precondition has since been met, which is why those features are
now enabled.** Stage 10 taught the hook to admit a behaviour-originated
dial by root policy; Stage 11 step 1 replaced the "every unticketed dial
is Kademlia's" assumption with real per-dial attribution, so an
unattributed dial is refused rather than misclassified
(`outbound_gate.rs::a_dial_no_behaviour_claimed_is_refused`). Only then
did `autonat`, `relay` and `dcutr` enter the feature list — and they
construct nothing yet.
