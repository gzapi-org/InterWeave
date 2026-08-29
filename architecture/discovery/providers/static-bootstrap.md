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

### The accepted address vocabulary

"Invalid multiaddress syntax" above needs a vocabulary to be checkable, and this is it. A configured entry is `/<host>/<value>/<transport>/<port>/p2p/<PeerId>`, where:

- **host** is one of `ip4`, `ip6`, `dns4`, `dns6`;
- **transport** is `tcp` — the only transport the substrate builds;
- **port** is `0..=65535`;
- an `ip4` value is a dotted quad, an `ip6` value is hexadecimal, and a DNS name is **not resolved** (see above).

This is a decision, not a description of what a multiaddr parser happens to accept, and it is deliberately narrower than the general multiaddr grammar: a profile naming a transport this build cannot dial is a configuration error an operator should read at validation, not a dial failure later. The set widens in the change that adds the transport — a new listen or dial capability and the configuration that may name it belong in the same commit.

It is spelled out in `interweave-profile-config` rather than delegated to the backend's parser. A configuration crate that pulled in a networking stack to name four protocols would invert the layering `crates/api` exists to hold, and would make the accepted set a property of a dependency rather than of this document.
