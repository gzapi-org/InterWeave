# cache

PeerCacheDiscovery implementation.

**Current status:** Stage 9, active workspace member. Stage 3 built the bounded persistence; Stage 9 added `PeerCacheDiscovery`, the `DiscoveryProvider` face of it — the one provider that persists observations (ADR-0027), fed by reachability hints travelling `ConnectionManager -> TransportRuntime -> here`. The `ObservedProtocol` hint class is deliberately refused until Stage 10 decides the capability-observation mapping in the architecture. Bounded advisory state on disk: it never dials, never grants trust, and holds no EndpointId, ChannelId, membership or presence. A file outside the bounded format is quarantined rather than parsed — the cache is disposable, so rejecting it is cheaper than defending it.
