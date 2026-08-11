# MdnsDiscovery

Purpose: optional zero-configuration LAN candidate discovery.

## Behavior

- local-link multicast only;
- normalize discovered PeerIds and addresses;
- honor expiry events/record TTLs;
- tolerate duplicate discover/expire sequences;
- enforce per-peer/global bounds before emitting candidates.

## Security

Any host on the multicast domain can advertise candidates. mDNS therefore grants **zero trust**. PeerTrustPolicy remains required before message delivery. LAN discovery also reveals that a P2P service exists; deployments with privacy requirements disable it.

## Failure

Networks may block multicast, containers may lack multicast routing, and interfaces may change. Such failures make this provider degraded/unavailable but do not kill transport or static/cache discovery.
