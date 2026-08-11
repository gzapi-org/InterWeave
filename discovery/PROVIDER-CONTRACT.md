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

A **known but unimplemented** provider is not treated as an unknown optional extension. If a minimum-v1 build recognizes `kademlia` in the schema but does not include its implementation, `enabled: true` is a hard validation/startup error. `enabled: false` may be accepted as a reserved disabled entry. The runtime must never silently start while omitting a provider the operator explicitly enabled.

## Upgradeability levels

v1 supports:

- compile-time implementation replacement: yes;
- config-time provider composition/order: yes;
- runtime enable/disable through validated config reload: design-supported, implementation may stage it;
- dynamic shared-library provider binaries: no.


## Libp2p-native provider adapters

A provider whose mechanism is itself a `NetworkBehaviour` still may not own the Swarm. Kademlia demonstrates the intended pattern: the Swarm task owns the concrete behavior and exposes a narrow bounded backend-internal control/event handle to the provider implementation. That handle is not part of the generic discovery contract and must not leak into transport consumers.
