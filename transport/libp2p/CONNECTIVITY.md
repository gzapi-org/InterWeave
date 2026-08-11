# Connectivity and NAT scope

## v1 target

Core v1 must work for:

- peers directly reachable over configured/listened TCP addresses;
- LAN peers learned through mDNS when enabled;
- peers reached after dialing static/cache-discovered public addresses.

## Protocol classification

| Mechanism | v1 role |
|---|---|
| TCP listen/dial | required core |
| Noise + Yamux | required core |
| Identify | required operational metadata / address observation |
| mDNS | optional LAN discovery |
| static bootstrap | discovery provider, not reachability authority |
| Circuit Relay v2 | optional implementation target after spike if remote deployments require relays |
| AutoNAT | deferred hardening |
| DCUtR / hole punching | deferred hardening |
| Kademlia | deferred discovery |

A bootstrap node and a relay are different roles. Configuring the same machine for both never makes bootstrap authoritative.

## v1 Internet statement

Initial deployments may require directly reachable peers or explicitly configured relay infrastructure. The project does not claim universal NAT traversal in v1. `SPIKE-004` measures relay and NAT needs before AutoNAT/DCUtR become required architecture.
