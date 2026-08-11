# StaticBootstrapDiscovery

Purpose: configured reachability entry points.

Example hint:

```text
/dns4/bootstrap.example.net/tcp/4001/p2p/<PeerId>
```

## Semantics

Static peers are candidate addresses with configured provenance. They are **not**:

- identity authorities;
- trust roots;
- membership servers;
- coordinators;
- brokers;
- message stores;
- channel owners;
- required permanent infrastructure after peers learn alternative reachability.

A configured bootstrap PeerId still requires an explicit trust rule before its application payload can reach Claude. Bootstrap entries may be intentionally untrusted data-plane peers used only to enter a wider network.

## Configuration

Default max entries: 64. Invalid PeerId/multiaddress pairs fail config validation. DNS resolution failures become provider health/diagnostics and are retried with bounded backoff by the appropriate lower layer.
