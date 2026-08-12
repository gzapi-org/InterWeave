# Rust workspace dependency boundaries

**Status:** Accepted; Model B modules added by ADR-0030.

## Context

Compile-time dependency direction is the strongest guard against leaking libp2p, Claude, or human-application concepts through transport contracts.

## Decision

Plan separate crates for neutral transport/EndpointId types, discovery API/providers, trust policy, runtime orchestration/EndpointRegistry, libp2p backend, endpoint-aware IPC, daemon/CLI, and bridge adapters. Human UI is an external application consumer of IPC and need not be a Rust crate.

No production crates are created in this architecture phase.

## Alternatives considered

Single crate; crate per tiny module; all crates depend on libp2p; dynamic provider SDK; human client linked directly to transport-libp2p.

## Consequences

More packages increase organization cost but preserve testability/feature gating. Endpoint routing policy stays in runtime, while wire codec stays in backend.

## Security implications

Identity/trust/network code stays isolated from Claude/human rendering. Admin IPC and data-plane IPC authority can be tested independently.

## Operational implications

Daemon/provider features remain platform-buildable; human/Claude clients can evolve independently behind IPC v2.

## Implementation implications

See rust blueprint. Enforce dependency graph and endpoint routing/property tests in CI.

## Revisit conditions

Revisit crate granularity for compile-time ergonomics without reversing boundaries.
