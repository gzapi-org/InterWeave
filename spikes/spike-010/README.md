<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Andrea Benetton -->
# SPIKE-010 — LAN multicast domain and mDNS discovery

The objective, the rows and the verdict are
[`architecture/roadmap/SPIKES.md`](../../architecture/roadmap/SPIKES.md)'s.
This directory holds the harness: **the flood row** (`harness/`), which
ADR-0053 D9 put first, measuring the mDNS crate's record store AS
RELEASED so that the bound the vendored copy adds is compared against a
number; and **the domain and node rows** (`domains.sh`, `node/`), below.
The bounds themselves are asserted by `crates/transport/libp2p/tests/
mdns_bounds.rs` in the same kind of namespace, on every CI run.

## The flood row

`harness/run.sh` builds the harness and runs it inside a private network
namespace (`unshare -rn`, no root) with one dummy interface carrying
multicast. That is a domain chosen for the run, the entry's rule for
evidence. The crate skips loopback interfaces, and on the development
host the shared interface does not loop multicast back. That was
measured on 2026-09-25: a probe sent to `224.0.0.251` over it never
arrived, while the same probe on loopback did.

The harness drives `libp2p::mdns::tokio::Behaviour` directly, with no
Swarm, and floods it with unsolicited responses. Each announces sixteen
distinct peers with one address each and a one-hour TTL. It stops each
stage when the store, read through the crate's public
`discovered_nodes()`, holds the target.

The run is committed beside the pin, as captured, under a 12-line
provenance header: `harness/REPRODUCTION-2026-09-25.log`. A later run is
compared against that file, not against this paragraph. What it shows,
2026-09-25, `libp2p-mdns 0.49.0`:

- **The store has no bound.** It holds every distinct announcement,
  65 536 of 65 536.
- **Each new record costs a scan of all the others.** The time spent
  inside `poll` per new record rose from about 3 µs at 256 to about
  213 µs at 65 536, so the total is quadratic: 10.5 s of `poll` for the
  last stage alone.
- **Resident memory grew about 14.5 MB over the run.** That covers the
  store and everything else the process holds, which the harness does
  not separate.

What it does NOT show:

- the two per-interface queues ADR-0053 also bounds, which are not
  visible through the public API;
- the query-flood amplification that D4's once-per-second rule
  addresses;
- anything about a real LAN. The domain rows are for that.

It uses one interface, IPv4 only.

## The domain and node rows

```
cd node && cargo build --release --locked        # the node, at the pin in node/Cargo.toml
NODE_BIN=node/target/release/node WORK=/some/scratch ./domains.sh all
```

`node/` is one `SwarmRuntime` with mDNS on, pinned to the workspace by
revision (af489d38) with the vendored `libp2p-mdns` patched in from the
same revision, and composed the way Stage 12's composer will be: the
runtime's `MdnsDiscovered`/`MdnsExpired` pushed into the mDNS provider,
both it and the static provider drained into a `DiscoveryManager`, and an
mDNS failure event mapped onto the provider's degraded state. It runs in
SPIKE-004 phase B's node image; the probes and drops use phase B's tool
image. The recorded run is `REPRODUCTION-2026-09-27.log`, beside this
file, every row in one pass; every number below is from it.

**Three domains, two bridges.** A rootless podman bridge carries
link-local multicast between its containers. What blocks it comes in
two shapes, which are not the same fact to a node:

- **path-blocked** (`mc-block`): every member drops multicast arriving.
  The send succeeds and nothing arrives, which to a node is exactly an
  empty LAN;
- **host-blocked** (one node on `mc-carry`): the node's own host drops
  its outgoing multicast, so its send fails with `EPERM` -- the failure
  ADR-0053 rule 5 surfaces as `MdnsInterfaceFailed`.

Turning an interface's multicast flag off blocks nothing: a send and a
join still succeed and the packet still arrives (measured by hand on
2026-09-27, before the rows were built, with the same probe the `env`
row uses; that run is not in the recorded log).

**The rows, as recorded (all PASS):**

| row | result |
| --- | --- |
| `env` | the carrying bridge delivers a plain UDP probe to 224.0.0.251:5353; the path-blocked one does not, the send succeeding -- and a unicast datagram to the same observer arrives on both, so the empty result is "nothing arrived", not "nobody listened" |
| `discover` | two nodes hold each other as candidates attributed to `mdns`, at each other's bridge address, through the provider and the manager; the mDNS provider healthy. Nothing mDNS learned reaches a dial (guarantee 13), both after the discovery (2001 ms < 15013 ms): a `DialPeer` finds NO address in the runtime's route book (`NoKnownAddress`); and around the control dial -- the address given through the command path, b not yet connected -- the root funnel, which counts every address any behaviour offers a dial, stays at 0 -> 0, so neither of the mDNS wrapper's two doors (its `NewExternalAddrOfPeer` swallow, its empty pending-dial answer) let an address through. The controls: that dial connects; and a dial while connected moves the funnel 0 -> 2, so the counter is live on this path. Learn-site counts `admitted=1` |
| `path` | on the path-blocked domain, 30 s (six query intervals): no `MdnsDiscovered` from the runtime, no pair refused by the provider, no candidate in the manager, no failure reported, the provider NOT degraded -- silence is not the degraded signal (`providers/mdns.md` §Failure) |
| `host` | the host-blocked node's runtime reports `MdnsInterfaceFailed { detail: "Operation not permitted" }`; the mDNS provider `Degraded` while the static provider stays `Healthy` and its candidate stands; the node connects to a peer and runs to its end |
| `crafted` | an unsolicited announcement naming a circuit address is refused at the learn site as `relayed` and never becomes a candidate; a lawful one from the same sender reaches the manager; counts `admitted=1 refused=relayed:1` |
| `ifchange` | a node whose interface is disconnected reports the removal (`NetworkChanged`) and, reconnected on a new address (10.89.0.3 -> 10.89.0.4, asserted different), rediscovers its peer, with no mDNS failure event since the reconnection (asserted; the disconnected window is not asserted either way) |

**The doors, proven by mutation.** Before this run was recorded, the
`discover` row was run against the node built from the pin with each
door opened in turn in a scratch copy of the tree: with the
`NewExternalAddrOfPeer` swallow removed, the control dial was offered 2
addresses (the two request-response behaviours); with the crate's
pending-dial answer forwarded, 1 (the crate's own); the row failed both
times. Those runs are not in the recorded log.

**Why the crafted address is a circuit.** The crate rewrites an
announced address's first host to the packet's observed source unless
that source is IPv6 link-local (`iface/query.rs`,
`_address_translation`), so a loopback literal announced from an IPv4
host reaches the learn site as the sender's own address. The circuit
marker survives the rewrite, so the boundary sees it.

**What these rows do not establish:**

- anything about a LAN population in the wild, or about interface change
  on real hardware (a container moved between networks is not a laptop
  leaving Wi-Fi);
- IPv6 (`ff02::fb`): every row is IPv4;
- the per-interface queues and the once-per-second reply rule, which
  `mdns_bounds.rs` asserts rather than these rows;
- that the `host` row's `EPERM` is what every host-level block produces:
  it is what an nftables output drop produces on this kernel.
