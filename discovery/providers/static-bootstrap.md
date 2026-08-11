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

A configured bootstrap PeerId still requires an explicit trust rule before ConnectionManager may establish/retain an ordinary v1 data-plane connection. Configuration does not grant trust.

## DNS ownership

`StaticBootstrapDiscovery` validates and emits configured `/dns4`/`/dns6` multiaddresses **without eagerly resolving them**. Its health covers configuration parsing, provider lifecycle, and its ability to emit configured candidate observations.

DNS resolution occurs when the libp2p/ConnectionManager dial path consumes the multiaddress. DNS lookup failures are therefore **dial/connection diagnostics**, not discovery-provider health failures. ConnectionManager applies its normal bounded retry/backoff policy. This separation avoids making the provider claim visibility into failures that occur only during dialing.

## Configuration

Default max entries: 64. Invalid PeerId/multiaddress syntax fails config validation. A valid DNS multiaddress whose hostname later fails to resolve does not invalidate the provider configuration; the dial attempt reports `PeerUnreachable`/address-resolution diagnostics as appropriate.
