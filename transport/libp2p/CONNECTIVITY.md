# Connectivity and NAT scope

## v1 target

Core v1 must work for:

- peers directly reachable over configured/listened TCP addresses;
- trusted LAN peers learned through mDNS when enabled;
- trusted peers reached after dialing static/cache-discovered public addresses.

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
| Kademlia | standard-v1 private peer-routing discovery when configured; default enabled, explicit opt-out |

A bootstrap node and a relay are different roles. Configuring the same machine for both never makes bootstrap authoritative.

## v1 Internet statement

Initial deployments may require directly reachable peers or explicitly configured relay infrastructure. The project does not claim universal NAT traversal in v1. `SPIKE-004` measures relay and NAT needs before AutoNAT/DCUtR become required architecture.

## Trust interaction

Reachability information is not authorization. mDNS/cache/static providers may learn candidates, but ordinary v1 connections are admitted/retained only for PeerIds separately authorized by `PeerTrustPolicy`; protocol-behaviour dial attempts are subject to the same root gate. A bootstrap address therefore is usable as an entry path only when its PeerId is also trusted; bootstrap configuration itself never changes trust.
