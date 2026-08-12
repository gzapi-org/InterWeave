# Separate config, identity, mutable state, cache, and runtime endpoints

**Status:** Accepted; endpoint state classes clarified by ADR-0030.

## Context

P2P identities are security-sensitive and multiple local profiles must never share them accidentally. Model B also distinguishes persistent endpoint configuration from ephemeral endpoint leases/presence.

## Decision

Use profile-specific platform directories for normal configuration (including endpoint definitions/default/ACLs), private identity key, mutable daemon state/logs, replaceable peer cache, and runtime socket/lock.

Endpoint leases and remote endpoint-directory results are runtime state only and are not persisted as authoritative configuration. Repository examples contain no private keys/secrets.

## Alternatives considered

Single loose state directory; key in YAML; environment-only identities; project repo key; persist endpoint leases/presence across restart.

## Consequences

Backup/deletion rules stay clear: endpoint config may be backed up; leases/directory cache are recreated. Daemon restart preserves PeerId but all local endpoint routes start offline until clients reconnect. The optional identity recovery record is stored **outside normal profile configuration/state** and is treated as private-key-equivalent offline backup material.

## Security implications

Private key remains owner-only. Standard v1 key-at-rest mode is filesystem-only; ADR-0038 records an explicit optional v2.x encrypted-key path rather than leaving it as an unnamed revisit. Recovery phrases are never written to config/state/cache/logs and never cross daemon IPC. Endpoint cache/leases cannot masquerade as durable authorization. Logs sanitize peer/endpoint identifiers as configured.

## Operational implications

Profiles migrate intentionally. Runtime sockets/leases/cache are disposable. Config schema v2 is the source of configured endpoint names/policies.

## Implementation implications

Atomic config writes, OS-specific directories, explicit profile initialization. Never auto-regenerate existing profile key; never restore old endpoint lease ownership from disk. Identity backup/restore follows ADR-0033 and requires offline exclusive identity-file access plus atomic owner-only writes.

## Revisit conditions

Revisit for HSM/keychain identities or stronger local endpoint-client authentication while preserving logical separation.
