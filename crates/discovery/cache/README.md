# cache

PeerCacheDiscovery implementation.

**Current status:** Stage 3, active workspace member. Bounded advisory state on disk: it never dials, never grants trust, and holds no EndpointId, ChannelId, membership or presence. A file outside the bounded format is quarantined rather than parsed — the cache is disposable, so rejecting it is cheaper than defending it.
