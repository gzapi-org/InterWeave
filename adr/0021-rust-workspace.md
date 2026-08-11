# Rust workspace dependency boundaries

**Status:** Accepted

## Context

Compile-time dependency direction is the strongest guard against leaking libp2p or Claude concepts through contracts.

## Decision

Plan separate crates for neutral contracts, discovery API/providers, trust policy, runtime orchestration, libp2p backend, IPC, daemon CLI, and a bridge adapter. No production crates are created in this architecture phase.

## Alternatives considered

Single crate; crate per tiny module; all crates depend on libp2p; dynamic provider SDK.

## Consequences

More packages increase build organization cost but enable targeted tests and feature gating. Avoid traits where modules do not vary independently.

## Security implications

Security-critical identity/trust/network code is isolated from Claude event formatting. Neutral contract crates have smaller dependency surfaces.

## Operational implications

Daemon/provider features can be built per platform. Diagnostics/CLI can depend downward without reversing core dependencies.

## Implementation implications

See rust blueprint. Enforce dependency graph in CI and use provider conformance tests.

## Revisit conditions

Revisit crate granularity during implementation if compile times/ergonomics suffer, while preserving dependency direction.
