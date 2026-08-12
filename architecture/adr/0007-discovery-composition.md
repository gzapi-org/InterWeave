# Concurrent discovery composition with configurable priority

**Status:** Accepted

## Context

LAN, cache, and configured bootstrap sources have different latency and scope. Sequential-only discovery creates unnecessary outages; fully undifferentiated sources lose useful provenance.

## Decision

Run enabled providers concurrently under DiscoveryManager. Merge by PeerId/address provenance. Provider priority/cost is configurable guidance for candidate selection, not a hard-coded sequence or trust weight. Active discovery intensity may back off when connectivity is healthy.

## Alternatives considered

Fixed provider order in transport core; first-provider-wins; run only one provider; provider-controlled dialing.

## Consequences

Duplicate observations become useful corroboration rather than duplicate peers. Manager complexity increases because expiry is per source/address.

## Security implications

Multiple independent sources improve resilience but do not raise authorization. Bounds prevent poisoning from consuming unlimited memory.

## Operational implications

Fast startup can use cache/static hints while mDNS runs. Wide-area providers can reduce traffic when enough diverse peers are connected.

## Implementation implications

Represent candidate address observations as a set of provenance records. Expire one source without deleting another source's address observation.

## Revisit conditions

Revisit priority heuristics after measured dial-success data; do not change the provider contract for tuning.
