<!-- SPDX-License-Identifier: Apache-2.0 -->
# tests/local-client-fake

**Current status:** Stage 14, active workspace member, test-only. Plan
§17 (2).

An in-memory binding of the four neutral traits (`DataSessionBinding`,
`DataSessionPort`, `AdminBinding`, `AdminPort`) over two nodes wired to
each other by `FakeNetwork::pair`. Each node has its own endpoint table,
lease table and per-session queues. It depends on `local-client-api`
and `transport-api` alone: reusing runtime code would test shared code
against itself.

## What it proves

It is the THIRD runner of `tests/local-client-conformance`
(`tests/fake.rs`, beside `in_process.rs` and `over_ipc.rs`): the same
generic functions, with no binding-specific branch. Every
local-semantics claim a client relies on holds here as it does on the
real bindings: exclusive leases with fresh epochs, the bounded queue and
acceptance that follows admission, the order of `events(max)`,
revocation notices, the admin overlay, and the directory's
advertised-and-leased rule. A client built against it is built against
something conformance has proved.

## What it cannot prove

- **Peer identity.** The identity a message carries is the configured
  one; "Noise proved the peer" is configuration here.
- **Trust.** The two nodes trust each other by construction.
- **The network's own outcomes.** `Timeout`, `PeerUnreachable`,
  `RemoteEndpointUnavailable` and `UnauthorizedPeer` are what a client
  is TOLD via `FakeNode::inject_send`. The fake never produces them
  itself.

So a client's handling of every outcome is proved TOTAL over
`TransportError`, not REACHABLE: which outcomes the network actually
produces stays with `tests/direct-v2` and the end-to-end suites.

`ipc-server`'s private `fake.rs` is a different thing (a call recorder
with fault hooks), and is not this crate.
