# Defer Kademlia from v1; future use only for network candidate expansion

**Status:** Accepted

## Context

Kademlia can expand an Internet-scale peer graph but adds bootstrapping, poisoning, Sybil/eclipse, convergence, and privacy complexity. Channel provider records would also conflate discovery with membership and expose namespaces.

## Decision

Do not require Kademlia in v1. If implemented later, bootstrap it from ordinary candidates and use bounded `get_closest_peers` queries against random namespace-independent keys/routing observations to diversify candidate peers. Do not use provider records for channel membership or channel-name discovery.

## Alternatives considered

Mandatory Kademlia; DHT provider records keyed by ChannelId; Kademlia as trust/membership database; omit Kademlia permanently.

## Consequences

v1 relies on configured remote entry points. Future Kademlia can be added without changing Claude or transport contracts.

## Security implications

Deferral reduces DHT attack surface. Future implementation still needs peer diversity, bounds, and distrust of DHT results.

## Operational implications

Operators have a simpler first deployment but less autonomous wide-area recovery.

## Implementation implications

Keep kademlia provider config/schema reserved but disabled/unsupported in minimum implementation until SPIKE-003.

## Revisit conditions

Revisit after the Kademlia discovery spike demonstrates network-size/recovery benefit and acceptable poisoning/privacy behavior.
