# Separate config, identity, mutable state, cache, and runtime endpoints

**Status:** Accepted

## Context

The Telegram reference separates plugin files and state; P2P identities are more security-sensitive and multiple local profiles must never share them accidentally.

## Decision

Use profile-specific platform directories for normal configuration, private identity key, mutable daemon state/logs, replaceable peer cache, and runtime socket/lock. Repository config examples never contain secrets or private keys.

## Alternatives considered

single state directory with loose permissions; key embedded in YAML; environment-only identities; project repository identity file.

## Consequences

More paths require clear diagnostics and migration tooling. The separation makes deletion/backup/permission rules explicit.

## Security implications

Private keys get owner-only data storage. Cache can be deleted without key loss. Logs are sanitized and never include secret configuration.

## Operational implications

Profiles can be backed up/migrated intentionally. Runtime sockets can be recreated freely. Config can be version controlled only after removing machine-local secrets.

## Implementation implications

Use OS-specific directory APIs; atomic writes; explicit profile initialization. Never auto-regenerate a missing key for an existing profile.

## Revisit conditions

Revisit storage integration if OS keychain/HSM-backed identities become necessary; preserve logical separation.
