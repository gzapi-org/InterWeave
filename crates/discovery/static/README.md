# static

StaticBootstrapDiscovery implementation.

**Current status:** Stage 9, active workspace member. Configured entries emitted as discovery candidates with configured provenance — never identity authorities, trust roots, membership servers, or permanent infrastructure (ADR-0010). Configuration does not grant trust: a configured PeerId still needs an explicit trust rule before ConnectionManager will hold an ordinary data-plane connection to it.

Addresses are validated and emitted **unresolved**. A `/dns4/` entry stays a name here; resolving it belongs to the dial path, which is what keeps a DNS outage a dial diagnostic rather than a discovery-provider health failure. That is ADR-0010's target and this crate's behaviour; what the BUILD does today is narrower, and the difference is recorded rather than left for a reader to hit: with no `dns` transport on the libp2p feature list such a dial fails structurally and the address is dropped from the book, so `profile-config` refuses a `/dns4` or `/dns6` host at validation until the `dns` transport is both on the feature list and built by the Swarm builder — the feature alone only makes it available (`architecture/discovery/providers/static-bootstrap.md`'s DNS-ownership section, and the plan's Stage 11 obligation).
