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
| `eim` | `masquerade` | one port for both destinations | endpoint-independent MAPPING |
| `eds` | `masquerade random` | a port per destination | endpoint-dependent MAPPING |

**Mapping only — filtering is neither configured nor measured.** RFC 4787
classifies a NAT by both, and conntrack gives endpoint-dependent
filtering in both rows here, so `eim` is a port-restricted cone rather
than a full cone. Whether a punch succeeds depends on filtering too, so
neither row licenses a claim about DCUtR succeeding; what they license
is a claim about the mapping it would face.

Measured, not asserted from the configuration. From the recorded run
below — the addresses are podman's default pool on one machine, so
expect different numbers and the same shape:

```
== NAT_MODE=eim ==
  kernel : 6.17.9-1.qubes.fc37.x86_64
peer private   : 10.89.1.3 45000
router public  : 10.89.0.4
observer 1 saw : 10.89.0.4 45000
observer 2 saw : 10.89.0.4 45000
VERDICT: ENDPOINT-INDEPENDENT MAPPING (one external port for both destinations)
== NAT_MODE=eds ==
observer 1 saw : 10.89.0.4 58519
observer 2 saw : 10.89.0.4 22699
VERDICT: ENDPOINT-DEPENDENT MAPPING (a port per destination)
2 matrix rows, each measured and matched, all distinct: eim eds
```

**`masquerade random` is per-flow randomisation, not per-destination
mapping by construction.** The two are observationally identical for
this probe and adequate for what DCUtR faces, but a real symmetric NAT
would still preserve a mapping consistently within its lifetime toward
one destination, and `random` does not. The row approximates the
property that matters and nothing more.

**The `eds` row is the one phase A could never reach.** On loopback every
punch succeeds, so `DCUTR.md` §13's cooldown, retry ceiling and
fallback-on-failure have never been exercised by anything. This is the
mapping behaviour a failing punch depends on.

**It is not yet a punch.** The topology has two NAT domains — two peers,
each behind its own router — which is the minimum a hole punch needs,
and an earlier version had one while claiming to be the environment a
punch requires. What is still missing is the relay both peers reach and
the nodes themselves; those arrive with steps 5 and 8.

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
- **an observed address that is not the router's** — translated, but by
  something other than the rule this harness installed, which the
  private-address control alone cannot see. The address is part of the
  mapping: two different external addresses sharing a port would
  otherwise have been reported as endpoint-independent;
- **a port that is not numeric** — a malformed observer line would make
  the address the "port", both would match, and any NAT would classify
  as `eim`;
- **a class that does not match `EXPECT`** — a NAT that silently behaves
  as the other class is a failure, not a footnote. With no `EXPECT` the
  probe prints `UNASSERTED` rather than passing quietly.

`run.sh` adds one more across rows: the measured classes must be
**distinct**, which fails if the topology built the same NAT twice or if
the classifier answers the same whatever it is shown.

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

The image is pinned by digest, not by tag. But **the image is not what
translates** — the NAT is the host kernel's netfilter, and the
`eim`/`eds` distinction is a `get_unique_tuple` port-selection behaviour
that has changed across kernel releases. Pinning the image and recording
nothing else aimed the reproducibility argument at the wrong component,
so `topology.sh` now prints the kernel, podman and nft versions into
every transcript. That is what makes a run attributable later.

`run.sh` also rebuilds the image every run rather than skipping when the
tag exists: `interweave-natmatrix:1` is exactly the moving tag the
Containerfile argues against, and a stale local copy would silently be
measured instead.
