<!-- SPDX-License-Identifier: Apache-2.0 -->
# `tests/connectivity`

**Current status:** Stage 5, active workspace member.

The Stage 5 exit gate: root connection and dial admission. Test-only —
nothing depends on this package.

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

**The behaviour-originated dial path is not yet closed**, and no test
here should be read as claiming it is. Stage 4's behaviour set is TCP,
Noise, Yamux and Identify, none of which dials, so there is nothing to
gate; the hook is `NetworkBehaviour::handle_pending_outbound_connection`
and it must require the same ticket before Kademlia, AutoNAT, Relay or
DCUtR is enabled (CLAUDE.md §3).
