<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Andrea Benetton -->
# SPIKE-010 — LAN multicast domain and mDNS discovery

The objective, the rows and the verdict are
[`architecture/roadmap/SPIKES.md`](../../architecture/roadmap/SPIKES.md)'s.
This directory holds the harness. **One row is built: the flood**, which
ADR-0053 D9 puts first, measuring the mDNS crate's record store AS
RELEASED so that the bound the vendored copy adds is compared against a
number. The domain rows are not built yet: a rootless podman bridge
that carries link-local multicast, one that blocks it, and the node
rows on each.

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
