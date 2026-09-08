<!-- SPDX-License-Identifier: Apache-2.0 -->
# SPIKE-004 phase B — the NAT matrix, in containers

Phase A ran on one machine over loopback, so the exit gate's
NAT/relay/hole-punch matrix is unmet and **Stage 11 cannot close**. This
directory is the environment that matrix needs, built with rootless
podman, and nothing more than the environment: it makes a real NAT whose
behaviour is chosen rather than inherited, and proves the NAT is what it
claims to be.

```
./run.sh          # every row, both domains: build, measure, assert, tear down
NAT_MODE=eds ./topology.sh up
EXPECT=eds ./probe.sh                                    # domain A
PEER=natm-peer-b ROUTER=natm-router-b LAN=natm-lan-b \
  EXPECT=eds ./probe.sh                                  # domain B
./topology.sh down
```

## What it establishes

**That the translation is real, in both NAT domains.** Peer A sends from
`10.89.1.3:45000` and the observers on the public side see `10.89.0.4`;
peer B sends from `10.89.2.3:45000` and they see `10.89.0.5` — each
router's own address. Phase A had no NAT at all, and the first thing
this harness owes is evidence that these ones do.

**That the mapping behaviour is the one that was asked for.** The two
rows are the two that decide whether a hole punch can work:

| `NAT_MODE` | nft rule | observers see | meaning |
| --- | --- | --- | --- |
| `eim` | `masquerade` | one port for both destinations | endpoint-independent MAPPING |
| `eds` | `masquerade random` | a port per destination | endpoint-dependent MAPPING |

**Mapping only — filtering is neither configured nor measured.** RFC 4787
classifies a NAT by both. The filtering here is READ rather than
measured: with a `nat postrouting` chain and no DNAT, an inbound packet
matching no conntrack entry is never reverse-translated, which makes
`eim` a port-restricted cone rather than a full cone. That is the same
reasoning-from-internals the `eds` paragraph below retracts, so it is
marked as read — a probe sending from a third address would measure it. Whether a punch succeeds depends on filtering too, so
neither row licenses a claim about DCUtR succeeding; what they license
is a claim about the mapping it would face.

Measured, not asserted from the configuration, and **for both NAT
domains**. The run below is complete rather than excerpted — an earlier
version of this section showed six of its lines under the heading "the
recorded run", and the lines it dropped were the two that name what each
router installed, which is where a router configured from the wrong
container's interface shows up first. (Not the only place: if the wrong
name matches nothing or matches the LAN side, that domain's peer leaves
untranslated and the probe fails the row with `NOT NATTED`. And when the
two containers happen to number their interfaces alike, the mistake is
neither visible nor harmful — which is why it survived a round.)

Captured from a terminal. `./run.sh > log 2>&1` reorders it: `run.sh`'s
own lines go to stdout, everything else to stderr, and bash block-buffers
the redirected stdout. The addresses are podman's default pool on one
machine, so expect different numbers and the same shape:

```

== NAT_MODE=eim ==
  router-a lan=10.89.1.2 pub=10.89.0.4
  natm-router: snat on eth1 using: masquerade
  router-b lan=10.89.2.2 pub=10.89.0.5
  natm-router-b: snat on eth0 using: masquerade
  NAT mode: eim (both domains)
  kernel : 6.17.9-1.qubes.fc37.x86_64
  podman : podman version 5.8.4
  nft    : nftables v1.1.3 (Commodore Bullmoose #4) (natm-router)
  nft    : nftables v1.1.3 (Commodore Bullmoose #4) (natm-router-b)
-- natm-peer behind natm-router --
peer private   : 10.89.1.3 45000
router public  : 10.89.0.4
observer 1 saw : 10.89.0.4 45000
observer 2 saw : 10.89.0.4 45000
VERDICT: ENDPOINT-INDEPENDENT MAPPING (one external port for both destinations)
-- natm-peer-b behind natm-router-b --
peer private   : 10.89.2.3 45000
router public  : 10.89.0.5
observer 1 saw : 10.89.0.5 45000
observer 2 saw : 10.89.0.5 45000
VERDICT: ENDPOINT-INDEPENDENT MAPPING (one external port for both destinations)

== NAT_MODE=eds ==
  router-a lan=10.89.1.2 pub=10.89.0.4
  natm-router: snat on eth1 using: masquerade random
  router-b lan=10.89.2.2 pub=10.89.0.5
  natm-router-b: snat on eth0 using: masquerade random
  NAT mode: eds (both domains)
  kernel : 6.17.9-1.qubes.fc37.x86_64
  podman : podman version 5.8.4
  nft    : nftables v1.1.3 (Commodore Bullmoose #4) (natm-router)
  nft    : nftables v1.1.3 (Commodore Bullmoose #4) (natm-router-b)
-- natm-peer behind natm-router --
peer private   : 10.89.1.3 45000
router public  : 10.89.0.4
observer 1 saw : 10.89.0.4 8518
observer 2 saw : 10.89.0.4 22070
VERDICT: ENDPOINT-DEPENDENT MAPPING (a port per destination)
-- natm-peer-b behind natm-router-b --
peer private   : 10.89.2.3 45000
router public  : 10.89.0.5
observer 1 saw : 10.89.0.5 42927
observer 2 saw : 10.89.0.5 21049
VERDICT: ENDPOINT-DEPENDENT MAPPING (a port per destination)

measured and matched: natm-peer=eim(45000,45000) natm-peer-b=eim(45000,45000) natm-peer=eds(8518,22070) natm-peer-b=eds(42927,21049)
```

Two things in it are worth reading twice.

**The summary carries the two OBSERVED PORTS per domain**, and they are
the only part of that line which is not a restatement of the input: the
peer names are literals, and the class cannot differ from the mode
because a mismatch exits the run before the summary is reached. `45000`
twice is one mapping for both destinations; `8518` and `22070` are two.

**`natm-router` is on `eth1` and `natm-router-b` on `eth0`.** Podman
numbers interfaces by walking each container's own network map, and the
two routers are attached to different pairs of networks, so nothing
makes the names agree — a later run had both on `eth0`. A shared
`configure_nat` deriving the interface from a hardcoded `natm-router`
therefore gave router B a rule matching its LAN side, translating
nothing, while the assertion beneath it passed: that assertion greps the
container for the string it just wrote there. The interface is derived
from the container being configured, which fails closed when no
interface there carries that address.

**`eds` is an approximation of a symmetric NAT, and how close is NOT
measured here.** What the probe establishes is the property DCUtR cares
about: one internal socket appears on a different external port to each
of two destinations. What it does not establish is the behaviour over
TIME toward ONE destination — the harness never sends twice to the same
observer, so it says nothing about whether the mapping is stable for the
life of a flow. An earlier version of this paragraph asserted that
`random` re-randomises per flow where a real symmetric NAT would not;
that was reasoning about netfilter internals rather than a measurement,
and conntrack's own entry makes the opposite the more likely reading.
Treat the row as the mapping property and nothing else.

**The `eds` row is the one phase A could never reach.** On loopback every
punch succeeds, so `DCUTR.md` §13's cooldown, retry ceiling and
fallback-on-failure have never been exercised by anything. This is the
mapping behaviour a failing punch depends on.

**It is not yet a punch.** The topology has two NAT domains — two peers,
each behind its own router — which is the minimum a hole punch needs,
and each is measured on every row rather than merely built: an earlier
version had one domain while claiming two, and its replacement built the
second and probed only the first. What is still missing is the relay
both peers reach and the nodes themselves; the relay server role is
step 6 of Stage 11, its client reservations step 5, and DCUtR step 8.

## How the measurement works, and why it is a comparison

One internal socket, bound to a fixed source port, sends to **two**
observers. A single observer cannot tell an endpoint-independent mapping
from a per-destination one, because there is nothing to compare against;
and two different sockets would be allocated two external ports under
any NAT, so the bound port is what makes the comparison mean anything.

`socat` reports the source it saw through `SOCAT_PEERADDR` /
`SOCAT_PEERPORT`, which is the entire measurement.

Five checks stand between the observation and a passing row — four
before the verdict is printed, one after — and any of them fails it.
(This said four, and before that two, both times because the sentence
was counted against the bullet list below it rather than against
`probe.sh`. It is the first bullet that kept going missing, and it is
the one that fires when the topology is up and nothing traversed it.)

- **an observer that saw nothing at all** — the topology is up, the
  probe ran, and no datagram arrived. Nothing is measured, so no class
  can be reported;
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

`run.sh` asserts nothing across rows, and that is deliberate. The
per-row `EXPECT` check already exits non-zero on a mismatch, so any
tally taken afterwards compares values that equal their own modes by
construction — two attempts at a cross-row claim have been vacuous for
exactly that reason, one of them a count of the loop it was written
over. What `run.sh` does check is its INPUT: `MODES` is a caller-supplied
row filter, and `MODES=" "` is set and non-null, so it used to run zero
rows and still print a passing summary. That check runs before the build
and is the only ASSERTION written in that file; everything else there —
the build, `topology.sh up`, each probe — fails the run by failing.

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
  steps 3 through 8, and five of the six evidence items `SPIKES.md`
  lists for phase B are blocked on components those steps have yet to
  build — everything but the NAT classes this directory measures. This is the
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
