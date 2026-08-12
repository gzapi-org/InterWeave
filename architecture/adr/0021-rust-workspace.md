# Rust workspace dependency boundaries

**Status:** Accepted; physical repository layout refined by ADR-0045.

## Context

Compile-time dependency direction is the strongest guard against leaking libp2p, Claude, or human-application concepts through transport contracts.

## Decision

Use the grouped crate/application boundaries in ADR-0045 and `docs/architecture/implementation-repository-layout.md`. Neutral transport/EndpointId, discovery, trust, local-client, IPC, and Kademlia-control contracts remain separate from concrete runtime/libp2p/platform/application crates. First-party desktop/Android human clients are Rust applications built from shared human-core/store/UI crates, while IPC remains language-neutral for future clients.

The repository now contains tracked crate/application **landing zones** plus an empty virtual Cargo workspace. No production crate manifests or Rust source are created by the layout commit.

## Alternatives considered

Single crate; crate per tiny module; all crates depend on libp2p; dynamic provider SDK; human client linked directly to transport-libp2p.

## Consequences

More packages increase organization cost but preserve testability/feature gating. Endpoint routing policy stays in runtime, while wire codec stays in backend.

## Security implications

Identity/trust/network code stays isolated from Claude/human rendering. Admin IPC and data-plane IPC authority can be tested independently.

## Operational implications

Daemon/provider features remain platform-buildable; human/Claude clients can evolve independently behind IPC v2.

## Implementation implications

See the Rust blueprint and ADR-0045. Add a crate to the root workspace only when its implementation phase creates the manifest/source. Enforce dependency direction and the layered test-placement rules in CI.

## Revisit conditions

Revisit crate granularity for compile-time ergonomics without reversing boundaries.
