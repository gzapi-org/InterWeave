<!-- SPDX-License-Identifier: Apache-2.0 -->
# SPIKE-004 phase B — the NAT matrix, in containers

Phase A ran on one machine over loopback, so the exit gate's
NAT/relay/hole-punch matrix was unmet — the NAT row has since been ruled
satisfied by this environment (2026-09-09, three deferrals; see the end
of this file), the relay and hole-punch rows have not — and **Stage 11
cannot close**. This
directory is the environment that matrix needs, built with rootless
podman, and nothing more than the environment: it makes a real NAT whose
behaviour is chosen rather than inherited, and proves the NAT is what it
claims to be.

**This directory is under the release-gate regime, not the frozen one**
(the preamble of `architecture/roadmap/SPIKES.md`, 2026-09-19). Phase A
is a frozen record: its harness pins this repository's crates at the
revision its verdict was measured against, and that pin never moves.
Phase B is a claim about what ships, so the node that will run here runs
the shipping substrate, pinned by revision like every spike, and that
pin moves only in the change that re-runs the rows and re-records the
verdict — never in a bump by itself, which is why this phase follows the
libp2p 0.57 bump rather than preceding it. The NAT rows below are not a
preliminary to that run: they are the harness's first rows, each with
its control, and no node is in them, so a substrate bump does not age
them — the kernel, podman and nft versions printed into every
transcript are what would.

```
./run.sh          # every mapping row, both domains, plus the default filtering row
FILTER_MODE=address-restricted MODES=eim ./run.sh   # a control for the classifier
FILTER_MODE=full-cone MODES=eim ./run.sh            # and the other one

# The manual path needs the image, which only run.sh builds -- without
# this, `topology.sh up` tries to pull a tag that exists nowhere and
# fails as a registry error rather than a setup one.
podman build -t interweave-natmatrix:1 -f Containerfile .
NAT_MODE=eds ./topology.sh up
EXPECT=eds ./probe.sh                                    # domain A, mapping
EXPECT_FILTER=apdf ./filter.sh                           # domain A, filtering
PEER=natm-peer-b ROUTER=natm-router-b LAN=natm-lan-b \
  EXPECT=eds ./probe.sh                                  # domain B, mapping
PEER=natm-peer-b ROUTER=natm-router-b LAN=natm-lan-b \
  EXPECT_FILTER=apdf ./filter.sh                         # domain B, filtering
./topology.sh down
```

## What it establishes

**That the translation is real, in both NAT domains.** Each peer sends
from its own LAN address and the observers on the public side see its
router's address instead; the two peers are seen as two different ones —
each router's own. The transcript below has the addresses, and they are
deliberately not repeated here: adding the filtering prober shifted
podman's pool by one and left this paragraph naming the wrong ones, and
the version after that stated the rule while still quoting two of them.
Phase A had no NAT at all, and the first thing this harness owes is
evidence that these ones do.

**That the mapping behaviour is the one that was asked for, and the
FILTERING behaviour too.** RFC 4787 classifies a NAT by both, and until
`filter.sh` existed this harness measured one and called the other a
non-goal — which is why no row licensed a claim about a punch at all.
Both are measured per domain now:

| `FILTER_MODE` | measured | which sources reached the peer |
| --- | --- | --- |
| `conntrack` | `apdf` — address-and-port-dependent, a port-restricted cone | the addressed endpoint only |
| `address-restricted` | `adf` — address-dependent | that address, any port |
| `full-cone` | `eif` — endpoint-independent | any source |

`conntrack` is the default and the honest row: it is what masquerade
alone gives and what a deployment actually meets. **The other two exist
so the classifier has a positive control for every branch** — with
conntrack alone it can only ever answer one way, and a classifier with
two unreachable branches is indistinguishable from a constant. They
install an nftables forward and need `NAT_MODE=eim`, since a static
forward cannot name a per-flow mapped port.

Both control rows are recorded, because a column headed "measured" owes a
run for every row in it and only the `conntrack` one had one. **These two
are EXCERPTS — the filtering lines only**, and are labelled as such
because the complete run below is the standard this directory holds
itself to; the rest of each is the same shape as that one. The two
`forward on` lines say which mode `configure_filtering` installed on
which router, and that it ran for both — an earlier version of these
excerpts left them out. They are NOT the evidence that the forward
landed correctly: two lines both reading `eth0`, as in the
`address-restricted` excerpt, cannot show the interface was derived per
container — the transcript's introduction below says so: when the two
containers number their interfaces alike the mistake is neither visible
nor harmful — the `full-cone` run recorded here happened to land router
B on `eth1`,
so that one does show it, by luck of podman's numbering rather than by
design. What fails closed either way is the pair of per-domain
verdicts beneath them — a forward on the wrong interface leaves domain B
measuring `apdf` against an `adf` expectation, and the row exits
non-zero. (A first version of this sentence called the two lines "the
only evidence", in the commit that removed three other claims the
transcript does not carry.) Re-recorded 2026-09-09 from the scripts as
committed; review findings on PR #81.

```
  natm-router: address-restricted forward on eth0 for udp/45000
  natm-router-b: address-restricted forward on eth0 for udp/45000
  filter  : address-restricted (both domains)
peer received  : CONTROL SAME_ADDRESS
VERDICT: ADDRESS-DEPENDENT FILTERING (an address-restricted cone: the address must match, the port need not)
peer received  : CONTROL SAME_ADDRESS
VERDICT: ADDRESS-DEPENDENT FILTERING (an address-restricted cone: the address must match, the port need not)
measured and matched: natm-peer=eim(45000,45000;45001,45001)/adf natm-peer-b=eim(45000,45000;45001,45001)/adf
```

```
  natm-router: full-cone forward on eth0 for udp/45000
  natm-router-b: full-cone forward on eth1 for udp/45000
  filter  : full-cone (both domains)
peer received  : CONTROL SAME_ADDRESS OTHER_ADDRESS
VERDICT: ENDPOINT-INDEPENDENT FILTERING (a full cone: any source reaches the mapping)
peer received  : CONTROL SAME_ADDRESS OTHER_ADDRESS
VERDICT: ENDPOINT-INDEPENDENT FILTERING (a full cone: any source reaches the mapping)
measured and matched: natm-peer=eim(45000,45000;45001,45001)/eif natm-peer-b=eim(45000,45000;45001,45001)/eif
```

**What the two together still do not license is a claim about a punch.**
They are the two NAT inputs; the third is the implementation — DCUtR's
address exchange and its timing — and no node runs here. So a row says
what a punch would face, not what it would do. That sentence took four
attempts to state without overreaching in one direction or the other,
so the counterexample stays: even `eds` on both sides does not entail
failure, because an endpoint-independent filter on either side forwards
the other peer's packet through the relay-created mapping whatever its
source. **That pairing is stated from RFC 4787, not measured here** —
the two control modes install a static forward and so need `NAT_MODE=eim`
(`topology.sh` refuses anything else), so this harness cannot build an
`eds` domain with an endpoint-independent filter, and the sentence is a
reason the mapping rows cannot decide the question rather than a row
of its own. The nodes, the relays and DCUtR run in the node rows below
(`nodes.sh`), which is where a punch is attempted.

Neither row rules an ATTEMPT out either: `DCUTR.md` §2 lists the
eligibility conditions and NAT class is not among them, and §9 requires
a NAT-induced FAILURE test — which is a punch attempted and failed, so
an `eds` mapping has to be reachable by an attempt for that test to
exist at all. An earlier version of this sentence said mapping decides
whether a punch can be attempted; it does not:

| `NAT_MODE` | nft rule | observers see | meaning |
| --- | --- | --- | --- |
| `eim` | `masquerade` | one port for both destinations | endpoint-independent MAPPING |
| `eds` | `masquerade random` | a port per destination | endpoint-dependent MAPPING |

**Filtering is measured, not read.** It used to be reasoned about here:
with a `nat postrouting` chain and no DNAT, an inbound packet matching no
conntrack entry is never reverse-translated, so `eim` is a
port-restricted cone rather than a full one. True, and the same
reasoning-from-internals the `eds` paragraph below retracts — so
`filter.sh` now sends the inbound instead. A third host aims a datagram
the peer never asked for at the mapped external port, and the class is
which sources arrive. Whether a punch succeeds depends on filtering too, and both are measured
here now — so what a row still does not license is a claim about the
punch, because the third input is the implementation and no node runs
here.

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

== NAT_MODE=eim FILTER_MODE=conntrack ==
  router-a lan=10.89.1.2 pub=10.89.0.5
  natm-router: snat on eth0 using: masquerade
  router-b lan=10.89.2.2 pub=10.89.0.6
  natm-router-b: snat on eth0 using: masquerade
  NAT mode: eim (both domains)
  filter  : conntrack (both domains)
  kernel : 6.17.9-1.qubes.fc37.x86_64
  podman : podman version 5.8.4
  nft    : nftables v1.1.3 (Commodore Bullmoose #4) (natm-router)
  nft    : nftables v1.1.3 (Commodore Bullmoose #4) (natm-router-b)
  socat  : socat version 1.8.1.3 on 26 Jun 2026 14:49:35 (natm-obs1)
-- natm-peer behind natm-router --
from port 45000
  peer private   : 10.89.1.3 45000
  router public  : 10.89.0.5
  observer 1 saw : 10.89.0.5 45000
  observer 2 saw : 10.89.0.5 45000
from port 45001
  peer private   : 10.89.1.3 45001
  router public  : 10.89.0.5
  observer 1 saw : 10.89.0.5 45001
  observer 2 saw : 10.89.0.5 45001
VERDICT: ENDPOINT-INDEPENDENT MAPPING (one external port for both destinations, in every trial)
peer private   : 10.89.1.3 45000
prober saw     : 10.89.0.5 45000
peer received  : CONTROL
VERDICT: ADDRESS-AND-PORT-DEPENDENT FILTERING (a port-restricted cone: only the addressed endpoint reaches the mapping)
-- natm-peer-b behind natm-router-b --
from port 45000
  peer private   : 10.89.2.3 45000
  router public  : 10.89.0.6
  observer 1 saw : 10.89.0.6 45000
  observer 2 saw : 10.89.0.6 45000
from port 45001
  peer private   : 10.89.2.3 45001
  router public  : 10.89.0.6
  observer 1 saw : 10.89.0.6 45001
  observer 2 saw : 10.89.0.6 45001
VERDICT: ENDPOINT-INDEPENDENT MAPPING (one external port for both destinations, in every trial)
peer private   : 10.89.2.3 45000
prober saw     : 10.89.0.6 45000
peer received  : CONTROL
VERDICT: ADDRESS-AND-PORT-DEPENDENT FILTERING (a port-restricted cone: only the addressed endpoint reaches the mapping)

== NAT_MODE=eds FILTER_MODE=conntrack ==
  router-a lan=10.89.1.2 pub=10.89.0.5
  natm-router: snat on eth1 using: masquerade random
  router-b lan=10.89.2.2 pub=10.89.0.6
  natm-router-b: snat on eth0 using: masquerade random
  NAT mode: eds (both domains)
  filter  : conntrack (both domains)
  kernel : 6.17.9-1.qubes.fc37.x86_64
  podman : podman version 5.8.4
  nft    : nftables v1.1.3 (Commodore Bullmoose #4) (natm-router)
  nft    : nftables v1.1.3 (Commodore Bullmoose #4) (natm-router-b)
  socat  : socat version 1.8.1.3 on 26 Jun 2026 14:49:35 (natm-obs1)
-- natm-peer behind natm-router --
from port 45000
  peer private   : 10.89.1.3 45000
  router public  : 10.89.0.5
  observer 1 saw : 10.89.0.5 33565
  observer 2 saw : 10.89.0.5 10750
from port 45001
  peer private   : 10.89.1.3 45001
  router public  : 10.89.0.5
  observer 1 saw : 10.89.0.5 19983
  observer 2 saw : 10.89.0.5 50695
VERDICT: ENDPOINT-DEPENDENT MAPPING (a port per destination)
peer private   : 10.89.1.3 45000
prober saw     : 10.89.0.5 26915
peer received  : CONTROL
VERDICT: ADDRESS-AND-PORT-DEPENDENT FILTERING (a port-restricted cone: only the addressed endpoint reaches the mapping)
-- natm-peer-b behind natm-router-b --
from port 45000
  peer private   : 10.89.2.3 45000
  router public  : 10.89.0.6
  observer 1 saw : 10.89.0.6 48810
  observer 2 saw : 10.89.0.6 22254
from port 45001
  peer private   : 10.89.2.3 45001
  router public  : 10.89.0.6
  observer 1 saw : 10.89.0.6 33304
  observer 2 saw : 10.89.0.6 7970
VERDICT: ENDPOINT-DEPENDENT MAPPING (a port per destination)
peer private   : 10.89.2.3 45000
prober saw     : 10.89.0.6 45641
peer received  : CONTROL
VERDICT: ADDRESS-AND-PORT-DEPENDENT FILTERING (a port-restricted cone: only the addressed endpoint reaches the mapping)

measured and matched: natm-peer=eim(45000,45000;45001,45001)/apdf natm-peer-b=eim(45000,45000;45001,45001)/apdf natm-peer=eds(33565,10750;19983,50695)/apdf natm-peer-b=eds(48810,22254;33304,7970)/apdf
```

Two things in it are worth reading twice, and one thing about it is
worth saying separately.

**The summary carries the OBSERVED PORTS per domain**, one group per
trial. They are the only part of that line which is not a restatement of
the input: the peer names are literals, and NEITHER class can differ from
what was asked for — the mapping class is checked against `EXPECT` and
the filtering class against `EXPECT_FILTER`, and either mismatch exits
the run before the summary is reached.
`45000,45000;45001,45001` is one external port per internal socket
whatever the destination, twice over; `33565,10750;19983,50695` is a
port per destination. Identical ports within a group are not a
degenerate reading — they are the observation that makes a row `eim`.

**The interface names are not a detail, and the run above shows why.**
Three of its four `snat on` lines say `eth0` and the fourth says `eth1` —
`natm-router` is on `eth0` in the `eim` row and `eth1` in the `eds` row,
which is the same router on two different interfaces within one run. So the
instability is visible in the record rather than asserted beside it: the
two routers land on different interfaces across runs, and one router
lands on different ones between the rows of a single run, because every
row recreates every container.

**This paragraph quotes the transcript, so re-recording it means
re-checking this sentence.** An earlier version said all four lines read
`eth0` — true of the run it was written against, false of the next one —
and the numbers in the paragraph above have been stale for the same
reason. Whichever interfaces a fresh capture shows, what the paragraph
must end up claiming is that they are not stable, which is the property
the per-container derivation exists for. Podman assigns them per container and per creation, and
the topology recreates every container for every row. So a shared
`configure_nat` deriving the interface from a hardcoded `natm-router`
gave router B a rule matching its LAN side, translating nothing, while
the assertion beneath it passed — that assertion compares against the
name the function itself wrote. The interface is derived from the
container being configured, which fails closed when no interface there
carries that address. (This paragraph pointed at the transcript for its
evidence while the transcript was re-recorded each round, which made the
claim true only on the runs that happened to show it.)

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
punch succeeds, so the failure cooldown (`DCUTR.md` §3 and
`transport/libp2p/CONNECTIVITY.md` §13 both set it at 5 min) and
fallback-on-failure have
never been exercised by anything. This row supplies the mapping such a
test would have to run against; it does not run one.

**The retry ceiling is not a document's**:
`DCUTR.md` has nine sections and no §13, which this cited for ten
rounds, and neither §3 nor that §13 sets a retry count — the PATH
matters, because `contracts/CONNECTIVITY.md` is a different document
with eleven sections and no §13, and it is the higher-authority one —
`SPIKES.md` records that the crate's `MAX_NUMBER_OF_UPGRADE_ATTEMPTS`
is `pub(crate)`, so DCUtR has no knob for it and the bound has to be
built by an adapter.

**The NAT rows are not a punch.** The topology has two NAT domains —
two peers, each behind its own router — which is the minimum a hole
punch needs, and each is measured on every row rather than merely built:
an earlier version had one domain while claiming two, and its
replacement built the second and probed only the first. The relays both
peers reach and the nodes themselves are the node rows' (below), which
put the shipping substrate on this topology.

## How the measurement works, and why it is a comparison

One internal address:port sends to **two** observers — two sequential
sockets binding the same port, which is the same thing as far as a NAT
is concerned, because RFC 4787 defines mapping behaviour over the
internal tuple rather than over a socket handle. A single observer
cannot tell an endpoint-independent mapping from a per-destination one,
because there is nothing to compare against; and two sockets on
DIFFERENT ports would be allocated two external ports under any NAT, so
the bound port is what makes the comparison mean anything.

`socat` reports the source it saw through `SOCAT_PEERADDR` /
`SOCAT_PEERPORT`, which is the entire measurement.

**And it is taken from more than one source port, which is what makes
`eim` an observation rather than a guess.** `masquerade random`
allocates per flow at random, so the two destinations can be handed the
same port by coincidence — roughly one row in 64512. A single pair would
then classify a correctly-built endpoint-dependent NAT as `eim`, failing
a good row under `EXPECT` and reporting the wrong class without it. Two
bound source ports are not a repeat of one look: endpoint-independence
must hold for every internal socket, so the second is a second instance
of the property. `eim` requires every trial to agree; one disagreement
is `eds`, which is the safe asymmetry — the harness cannot talk itself
into the more permissive mapping class.

Five checks stand between the observation and a passing row — four
before the verdict is printed, one after — and any of them fails it.
The four before the verdict run on EVERY trial, not only the last; the
fifth is taken once, on the class the trials produced. ("The class
every trial agreed on" was true only for `eim`: `eds` needs no
agreement, because one disagreeing trial imposes it on its own. The
correction that first replaced that phrase said the opposite — that
`eds` was the class no single trial produced — which denies the safe
asymmetry stated in the paragraph immediately above it.)
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

`probe.sh` validates its own trial list the same way, and for the same
reason: `class` starts at `eim` and only a disagreeing trial moves it,
so a list naming nothing would report ENDPOINT-INDEPENDENT and exit 0
having measured nothing. It requires at least two tokens; each numeric,
with no more than five digits once leading zeros are stripped — checked
BEFORE the arithmetic, because a twenty-digit token wraps under
`$((10#…))` and `18446744073709551617` wraps to 1, which a range check
after the arithmetic admits (an earlier version of this sentence said
such a token "wraps negative" and credited the range check with
refusing it; it does not) — and in 1–65535, because `*[!0-9]*` admits
`0` and `70000`, and `bind=:0` binds ANY port, which breaks the premise
that both sockets share one internal tuple and measures a correct `eim`
topology as `eds`; and
distinct once normalised, since `45000` and `045000` are two spellings
of one tuple and one trial re-opens the coincidence the trials exist to
close.

`run.sh` asserts nothing across rows, and that is deliberate. The
per-row `EXPECT` check already exits non-zero on a mismatch, so any
tally taken afterwards compares values that equal their own modes by
construction — two attempts at a cross-row claim have been vacuous for
exactly that reason, one of them a count of the loop it was written
over. What `run.sh` does check is its INPUT: `MODES` is a caller-supplied
row filter, and `MODES=" "` is set and non-null, so it used to run zero
rows and still print a passing summary. That check runs before the build.
It is not the only one — `FILTER_MODE` is validated beside it, also
before the build, and the summary's ports and filtering class are
asserted per domain — and the
comment in `run.sh` stops counting, because three successive versions of
that sentence were each falsified by the next commit to add a check.

## The node rows

```
cd node && cargo build --release --locked        # the node, at the pin in node/Cargo.toml
NODE_BIN=node/target/release/node WORK=/some/scratch ./nodes.sh all
```

`nodes.sh` puts the shipping substrate on this topology. `node/` is one
`SwarmRuntime` configured from flags, pinned to the workspace by
revision -- 680bbe10 now, recorded by the commit that added
`REPRODUCTION-2026-09-26-rebuild.log`; 6500391e, recorded by bd0ab554,
for the rows run before the rules below -- with the vendored AutoNAT and mDNS crates patched
in from the same revision (a patch table does not cross a git
dependency). Its `Cargo.lock` is the root lock at the pin, plus only the
node itself and `signal-hook-registry`, so the graph is the shipping
one. It runs in `Containerfile.node`'s glibc image, which ties the
binary to a host whose glibc is no newer than the image's (2.42).

Each row brings the matrix up afresh, starts two relays that are also
AutoNAT servers and verify EACH OTHER, and runs its clients behind the
routers. A row asserts its claim against a control and prints `PASS`,
or records numbers against a document and prints `MEASURED`. The
recorded run is `REPRODUCTION-2026-09-26.log`, beside this file: every
row in one pass of `nodes.sh` at fa62ad86, pinned at 6500391e. The two
rows the rules below changed, `early` and `ifchange`, were re-run at
680bbe10 after them and ASSERT them now:
`REPRODUCTION-2026-09-26-rebuild.log`. Every number below is from one of
the two, named where it is the second, unless it is labelled otherwise.

**Four facts about the harness had to hold before a row meant
anything.** Each was found by a run before the recorded one, which
measured the wrong thing first; those runs' logs are not committed.

- **The public side is a public range** (`PUB_SUBNET`, 11.0.0.0/24 in
  the node rows' own namespace). AutoNAT probes only an address
  `is_probeable_address` calls public; podman's pool is RFC 1918; and a
  relay hands out a reservation's addresses only once AutoNAT has
  verified one.
- **The LANs are unreachable from outside them.** Rootless podman
  routes between every bridge, and punches crossed LANs over the peers'
  private addresses without touching a NAT. Every public-side node, and
  each router toward the other LAN, now blackholes that LAN's subnet, as
  podman assigned it.
- **The routers hold an unsolicited SYN silently**, as RFC 5382 REQ-4
  requires of a NAT. A Linux router answers one with a RST, which
  refused simultaneous opens.
- **Each punch trial has a port of its own.** With every trial on port
  4001, the same node binary punched `eim` 1 of 10 in
  `REPRODUCTION-2026-09-26-port-control.log` (the punch row as it stood
  at 8ee1ec5f), against 10 of 10 in the recorded run. The two runs
  differ in that port and, not by design, in one more thing: router A's
  public side came up as `eth1` in both failing runs and as `eth0` in
  the passing one, since podman orders a container's interfaces as it
  attaches them. So the port is the LIKELY cause, not an isolated one --
  the port-control log's header says "isolates", which is stronger than
  the two runs support (#127's review). WHY a port would matter is an
  inference no log records: the routers' connection tracking outlives a
  trial, so a later trial's mapping to the relay cannot reuse a port an
  earlier trial's flow still holds.

**The five items, as recorded:**

| item | row | result |
| --- | --- | --- |
| two relay and probe services | `services` | PASS: each relay verified by the other's probe; a probe of the client refused by its NAT (which server probes is the client crate's random pick); a reservation on each relay carrying its verified address; the client never verified public |
| relay loss | `loss` | PASS: r1 killed; after the kill, the loss reported and standing one of two; a dialer behind router B reaches the client over r2, while the same dialer through r1 fails |
| capacity denial | `capacity` | PASS: r1 at a ceiling of one accepts one client, and every denial names the other (1, `ResourceLimitExceeded`); both hold r2; the control, a ceiling of two, denies nobody |
| network-interface change | `ifchange` | PASS (re-run at 680bbe10): the change is reported (`NetworkChanged`, removed, then added); at the removal the client closes its connection to each relay within ten seconds, and each relay logs its end of it going within the 120 s window (that the relay's keepalive is what ended it is an inference: nothing else there sends on an idle connection); after reconnection on a new address both reservations are rebuilt within 50 s and a dialer behind router B reaches the client through a relay (1 connected, 0 dials failed). As first recorded at 6500391e it was MEASURED, and NOT MET: nothing was rebuilt in 120 s after either step, and a dialer through each relay failed (0 connected, 2 failed) |
| hole-punch success rates | `punch` | MEASURED: endpoint-independent mapping 10 of 10, endpoint-dependent 0 of 10, a success counted only with the Relayed-to-Direct `HolePunched` path change |
| resource cost | `cost` | MEASURED at r1's defaults: idle 11.6 MB resident, 15 descriptors; 64 reservations (the ceiling) 15.2 MB, 78 descriptors; plus 64 circuits 17.5 MB, 78 descriptors; one client with its reservation and both ring circuits 11.4 MB, 13 descriptors. About 56 KB and one descriptor per reservation, 37 KB and none per circuit |

**Five findings, each a number against a document:**

1. **A relay served before AutoNAT verified it, and its unusable
   reservations held its ceiling** (`early`, at 6500391e). FIXED by
   RELAY.md section 8's hop gate and re-run at 680bbe10: two clients
   asking early against a ceiling of two, r1 granted nothing before its
   verification at 40.0 s, denied no ask for room, and both clients then
   held a usable reservation on it -- PASS. As first recorded:
   - The server forced `Status::Enable` with nothing above it, so it
     accepted reservations carrying no address. The client refused each
     (`NoAddressesInReservation`).
   - The client re-asked over the same connection, which the crate reads
     as a renewal. So the per-peer ceiling is skipped, and the total is
     checked with every client's phantom counted in it.
   - Two clients against a ceiling of two: the first phantom at 1.6 s;
     8 asks denied, all `ResourceLimitExceeded`, from 7.6 s to 89.8 s;
     r1 verified at 35.0 s and granted nothing between that and its
     last denial; the first usable reservation at 168 s of the client's
     run.
   - The control, a ceiling of three, denied nobody: 2 accepted, 5
     renewed. So it is the phantoms holding the total, not another
     limit the same status covers.
2. **A network change rebuilt nothing** (`ifchange`, at 6500391e).
   FIXED by transport/libp2p/CONNECTIVITY.md section 14 item 5 --
   close-on-removal and a keepalive at both ends of a relay control
   connection -- and re-run at 680bbe10: PASS, the table's row above.
   As first recorded:
   - `contracts/CONNECTIVITY.md` says a network change invalidates
     affected evidence and rebuilds relay reservations.
   - As then built, step 10 reported the change and closed nothing, and
     the substrate ran no keepalive. So an idle connection over a
     vanished interface was seen by neither end.
   - After reconnection, a dialer through either relay could not reach
     the client, which still counted both relays as connected.
3. **RELAY.md section 8 misdescribes the pinned relay's rate limiters**
   (`ratelimit`).
   - It reads them as "sixty per IP per minute". The crate's default is
     a token bucket per IP holding 60 and adding one per minute, for
     reservations and circuits alike.
   - Measured two ways. 120 dials from one address: 60 circuits accepted
     in the first minute, and 120 outcomes denied `ResourceLimitExceeded`
     (the status a full table gives too). The log counts outcomes, not
     dials: that 60 denied dials produced 120 denials through the
     runtime's retries is an inference.
   - Four minutes later, 10 fresh dialers from the same address: 4
     accepted and 7 denied. A bucket refilled one a minute admits about
     four; "sixty per minute" would admit all ten.
   - A relay behind one shared address, or serving a carrier's CGNAT,
     gets 60 circuits per address and then one a minute.
4. **The circuit ceiling of 128 is out of reach from two source
   addresses within the hour** (`cost`), for the reason above.
   - The cost row measures 64 circuits.
   - The per-circuit cost is what extrapolates to the ceiling: about
     4.7 MB for 128 circuits, at 37 KB each.
5. **Seating 64 clients from two NAT addresses took 62 s**, with no
   denial. That InterWeave's own pre-Noise budget (30 starts per
   source-address bucket per minute, `resource-limits.md`) is what paces
   it is an inference, not a measurement: the log carries no pre-Noise
   refusal, only a rate that fits it.

The run this one replaces reported a sixth finding, that a TCP punch
through `eim` mostly fails (2 of 10). That was the harness, most likely
its shared port: the fourth fact above, its control log, and the
confounder that log did not remove.

**What the node rows do not establish**, beside the list below:

- success rates against the NAT population in the wild (these are rates
  against two built classes);
- a mixed pairing (`topology.sh` gives both domains one `NAT_MODE`);
- QUIC (the substrate is TCP only);
- independently operated services;
- that a punch's direct connection crossed the routers' public
  addresses: the rows check the path change, and the blackholes are what
  leave no other route.

Whether the five items close, and on which of these limits, is the
owner's decision and architect-cto's record, not this file's.

## What it does NOT establish

Phase B as the plan states it bundles several claims. The NAT rows
answer part of one — the NAT classes, in both the mapping and the
filtering half — and the node rows below the other five, each with the
limits it states. Everything below is outside what either can say.

- **Hole-punch success RATES.** A property of the NAT population in the
  wild. This shows the mechanism works against a class; it cannot say
  what fraction of real peers are in that class.
- **A specific carrier's CGNAT.** Port-block allocation, rebinding
  intervals and ALGs are operator behaviour. `eds` approximates the
  mapping property that matters and nothing else.
- **Real interface-change events.** Moving a container between networks
  is not a laptop leaving Wi-Fi.
- **Independently operated services.** The node rows' two relays and
  probe servers are two processes this harness runs, on one host, under
  one operator: RELAY.md section 7's independence is operational and no
  container can show it.

**So this does not close phase B by itself.** Whether a containerised matrix can
satisfy the exit gate's NAT row — with the population claim, the public
VM and a carrier's CGNAT explicitly deferred, as Stage 9 deferred mDNS
and Stage 10 the release gate — was the owner's decision, and the owner
ruled on 2026-09-09 that it does, with those three deferrals recorded;
the plan's Stage 11 section carries the ruling and the five items it does
not settle. (This is the third copy of that deferral list;
`SPIKES.md` and the plan carry the other two. Those lost "the filtering
half" when `filter.sh` landed (`0c5f7e1`) and one commit later
(`32a7f8f`); this one was extended in `8bbaa75`, after three attempts at
counting the rounds between had each been wrong — so the commits are
named and the rounds are not.)

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

The image is pinned by digest, and its three packages by version. But
**the image is not what translates** — the NAT is the host kernel's
netfilter, and the
`eim`/`eds` distinction is a `get_unique_tuple` port-selection behaviour
that has changed across kernel releases. Pinning the image and recording
nothing else aimed the reproducibility argument at the wrong component,
so `topology.sh` now prints the kernel, podman and nft versions into
every transcript. That is what makes a run attributable later.

`run.sh` also rebuilds the image every run rather than skipping when the
tag exists: `interweave-natmatrix:1` is exactly the moving tag the
Containerfile argues against, and a stale local copy would silently be
measured instead.
