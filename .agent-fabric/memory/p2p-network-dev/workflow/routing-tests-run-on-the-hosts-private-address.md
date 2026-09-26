---
role: "p2p-network-dev"
class: workflow
topic: "routing-tests-run-on-the-hosts-private-address"
description: "ADR-0052's floor refuses loopback from a peer, so any test that needs a peer's ADVERTISED address to be learned or routed must listen on the host's private address; and an absence-only test goes vacuous when the sibling that was its…"
tier: 1
knowledge_scope: full
distilled_at: "2026-09-26"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 18201d3eab65baac
---

## ADR-0052's floor refuses loopback from a peer, so any test that needs a peer's ADVERTISED address to be learned or routed must listen on the host's private address; and an absence-only test goes vacuous when the sibling that was its control breaks

ADR-0052 refuses loopback whoever supplies it, and since A 2026-09-20
that covers Identify's `listen_addrs` into the book and Kademlia's
routing table. So a two-runtime test on `127.0.0.1` never learns or
routes the other peer. On PR #111 five Kademlia tests timed out
"waiting for ... routed" for exactly this reason (`8a1d98a`).

**The pattern**, first in `tests/connectivity/tests/dcutr.rs`:
`private_interface_v4()` reads the host's RFC 1918 address off an
unconnected UDP socket (`connect("10.255.255.255:9")`, no packet sent),
the runtimes listen there, and rule 3 admits the peer's private address
beside a private listener of the same family. A host without one stands
the test DOWN with an `eprintln!` rather than passing it — ADR-0052 has
no test-only knob to admit loopback. This host is `10.137.0.2`; the
hosted CI runners have one too. The helper is copied in three test
files today (`dcutr.rs`, `kademlia_driver.rs`, `overlay_health.rs`);
moving it to `tests/support` is an open follow-up.

**The trap that is not a failure.** `two_network_ids_never_mix` asserts
only an ABSENCE — a foreign network's peer is never routed — and its
control was implicit: sibling tests proving routing DOES happen on the
same setup. When the siblings broke, it kept passing, now for the wrong
reason. Moving the whole file restored its control; collapsing the
network namespace (`kad_protocol` ignoring `network_id`) makes it fail
again, which is the proof.

**How to apply:** when a change makes tests fail, read the tests in the
same file that PASSED, and ask of each absence assertion what now
guarantees the thing it forbids could still have happened.

**And at the moment a learn-site hook is WRITTEN, not after CI:** grep
the wire tests for every test that exercises THAT learn path and check
its listen address. Knowing this note did not stop it recurring — on
#111 the relay learned-relay hook (`03e5b11`) broke
`relay_client.rs`'s learning test the same way (relay on loopback, never
learned), found only by the full CI run (`b74c8a0`). And `xtask ci`
STOPS at the first failing test binary, so its tally is partial: rerun
`cargo test --workspace --all-targets --no-fail-fast` for the real one.
Move a control peer to the private address along with the subject, or
the control passes for being loopback rather than for what it tests.
`tests/kademlia/tests/opt_out.rs` stayed on loopback because neither of
its tests depends on routing and both carry their own controls.

*Observed 2026-09-25 (p2p-network-dev)*
