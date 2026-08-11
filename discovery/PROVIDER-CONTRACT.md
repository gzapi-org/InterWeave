# DiscoveryProvider rationale and contract notes

The normative behavioral contract is [../contracts/DISCOVERY.md](../contracts/DISCOVERY.md).

## Why a trait is justified

What varies independently: mechanism, source lifecycle, configuration, expiry semantics, and network scope.

Consumers: only `DiscoveryManager`.

Information crossing boundary: normalized candidate identity, addresses, provenance/expiry, provider health.

Failure crossing boundary: provider-local error/health state; never a Swarm panic or transport shutdown.

This is enough independent variation to justify a v1 trait. The trait does not expose a Swarm or generic `Any` configuration bag.

## Configuration dispatch

Configuration uses a typed tagged enum in the composition layer:

```text
provider.type = peer-cache | mdns | static-bootstrap | kademlia
provider.config = provider-specific validated schema
```

Adding a provider requires registering a new type and schema in the daemon build, not modifying the transport consumer. Unknown provider types fail validation unless explicitly marked optional for forward-compatible fleet rollout.

## Upgradeability levels

v1 supports:

- compile-time implementation replacement: yes;
- config-time provider composition/order: yes;
- runtime enable/disable through validated config reload: design-supported, implementation may stage it;
- dynamic shared-library provider binaries: no.
