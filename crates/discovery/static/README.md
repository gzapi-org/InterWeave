# static

StaticBootstrapDiscovery implementation.

**Current status:** Stage 9, active workspace member. Configured entries emitted as discovery candidates with configured provenance — never identity authorities, trust roots, membership servers, or permanent infrastructure (ADR-0010). Configuration does not grant trust: a configured PeerId still needs an explicit trust rule before ConnectionManager will hold an ordinary data-plane connection to it.

Addresses are validated and emitted **unresolved**. A `/dns4/` entry stays a name here; resolving it belongs to the dial path, which is what keeps a DNS outage a dial diagnostic rather than a discovery-provider health failure.
