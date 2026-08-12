# Compile-time discovery providers with configuration composition

**Status:** Accepted

## Context

The requirement is upgradeability, not dynamic binary plugins. Rust dynamic ABI/plugin loading creates security/versioning complexity with little v1 value.

## Decision

Support replaceability through a Rust trait, compile-time provider registry, namespaced typed config, and config-driven composition. Design runtime enable/disable/restart, but do not load arbitrary shared libraries in v1.

## Alternatives considered

hard-coded providers; dynamic `.so/.dll` providers; separate provider processes from day one.

## Consequences

New provider code requires a daemon build/update but not a transport/Claude redesign. Configuration can select which built providers run.

## Security implications

Avoids executing arbitrary provider binaries inside the network daemon. Provider input remains untrusted and conformance-tested.

## Operational implications

Fleet upgrades remain conventional package upgrades. Provider configuration versions can migrate independently when needed.

## Implementation implications

Use tagged enums/factory registry at composition edge; stable discovery trait for behavior. Unknown required provider types fail config validation.

## Revisit conditions

Revisit if third-party provider ecosystems or independently deployable enterprise connectors become a concrete requirement.
