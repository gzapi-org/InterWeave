<!-- SPDX-License-Identifier: Apache-2.0 -->
# SPIKE-004 phase B — the NAT matrix, in containers

Phase A ran on one machine over loopback, so the exit gate's
NAT/relay/hole-punch matrix is unmet and **Stage 11 cannot close**. This
directory is the environment that matrix needs, built with rootless
podman, and nothing more than the environment: it makes a real NAT whose
behaviour is chosen rather than inherited, and proves the NAT is what it
claims to be.

```
./run.sh          # every row: build, measure, assert, tear down
NAT_MODE=eds ./topology.sh up && EXPECT=eds ./probe.sh
./topology.sh down
```

## What it establishes

**That the translation is real.** The peer sends from `10.89.1.3:45000`
and the observers on the public side see `10.89.0.4` — the router's
address. Phase A had no NAT at all, and the first thing this harness
owes is evidence that this one does.

**That the mapping behaviour is the one that was asked for.** The two
rows are the two that decide whether a hole punch can work:

| `NAT_MODE` | nft rule | observers see | meaning |
| --- | --- | --- | --- |
| `eim` | `masquerade` | one port for both destinations | endpoint-independent; DCUtR should succeed |
| `eds` | `masquerade random` | a port per destination | endpoint-dependent; DCUtR must fail and fall back to the relay |

Measured, not asserted from the configuration: `eim` gave `45000` to both
observers and `eds` gave two different ports in the same run.

**The `eds` row is the one phase A could never reach.** On loopback every
punch succeeds, so `DCUTR.md` §13's cooldown, retry ceiling and
fallback-on-failure have never been exercised. This is the topology that
exercises them.

## How the measurement works, and why it is a comparison

One internal socket, bound to a fixed source port, sends to **two**
observers. A single observer cannot tell an endpoint-independent mapping
from a per-destination one, because there is nothing to compare against;
and two different sockets would be allocated two external ports under
any NAT, so the bound port is what makes the comparison mean anything.

`socat` reports the source it saw through `SOCAT_PEERADDR` /
`SOCAT_PEERPORT`, which is the entire measurement.

Two checks run before the verdict, and either fails the row:

- **an observer that saw the peer's private address** — no translation
  happened, and every conclusion would be a loopback result under a
  phase-B heading;
- **a class that does not match `EXPECT`** — a NAT that silently behaves
  as the other class is a failure, not a footnote.

## What it does NOT establish

Phase B as the plan states it bundles two different claims, and this
harness answers one of them.

- **Hole-punch success RATES.** A property of the NAT population in the
  wild. This shows the mechanism works against a class; it cannot say
  what fraction of real peers are in that class.
- **A specific carrier's CGNAT.** Port-block allocation, rebinding
  intervals and ALGs are operator behaviour. `eds` approximates the
  mapping property that matters and nothing else.
- **Real interface-change events.** Moving a container between networks
  is not a laptop leaving Wi-Fi.
- **Anything about InterWeave itself.** No node runs here yet. The
  behaviours this matrix exists to test — AutoNAT, Relay, DCUtR — are
  steps 3 through 8, and three of phase B's own evidence items are
  blocked on components those steps have yet to build. This is the
  environment, ready for them.

**So this does not close phase B**, and whether a containerised matrix
can satisfy the exit gate's NAT row — with the population claim
explicitly deferred, as Stage 9 deferred mDNS and Stage 10 the release
gate — is the owner's decision and belongs in the plan, not here.

## Notes for whoever extends it

Three things cost time to find, all of which fail silently:

- **`podman exec` does not attach stdin without `-i`.** `nft -f -` then
  reads EOF, installs nothing, and exits 0. The first version of
  `topology.sh` reported a NAT it had not built; the assertion that the
  rule is present afterwards is what makes that visible.
- **`podman inspect`'s per-network object has no interface name** in
  5.8. Asking for one yields an empty string, and `oifname ""` is a rule
  that matches nothing. The interface is derived by matching the address
  instead.
- **`socat`'s `SYSTEM:` reads a colon as its own separator**, so an
  `addr:port` format string is rejected as "wrong number of parameters"
  — visible only in the container's logs. The observers report space
  separated.

The image is pinned by digest, not by tag: this harness's whole output
is a claim about what a NAT did, and a tag that moves under it
invalidates the record without changing a line here.
