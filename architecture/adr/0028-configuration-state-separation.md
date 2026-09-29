# Separate config, identity, mutable state, cache, and runtime endpoints

**Status:** Accepted; endpoint state classes clarified by ADR-0030.

## Context

P2P identities are security-sensitive and multiple local profiles must never share them accidentally. Model B also distinguishes persistent endpoint configuration from ephemeral endpoint leases/presence.

## Decision

Use profile-specific platform directories for normal configuration (including endpoint definitions/default/ACLs), private identity key, mutable daemon state/logs, replaceable peer cache, the profile lock (in the state directory), and the runtime sockets (A 2026-09-28).

Endpoint leases and remote endpoint-directory results are runtime state only and are not persisted as authoritative configuration. Repository examples contain no private keys/secrets.

The profile lock is one file in the STATE directory (`<state>/profile.lock`, owner-only), reachable offline with no runtime directory, held exclusively by the daemon for its lifetime and by `transportctl identity backup`/`restore`; it is released, never unlinked, so no two processes can lock different inodes at one path. The runtime sockets are disposable and removed only by the lock holder, and only when the path holds a socket owned by the daemon's uid; anything else at that path is fatal. Endpoint enable/disable/default changed over administrative IPC is a RUNTIME OVERLAY over the configured state, never written back to `config.yaml`, and does not survive a restart. (A 2026-09-28)

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

## Amendments

Full notes: [`history/0028-amendments.md`](./history/0028-amendments.md).

| Date | Amendment | Effect |
|---|---|---|
| 2026-09-28 | The profile lock lives in the state directory and is released, never unlinked; stale sockets go only under the lock; admin endpoint mutations are a runtime overlay | Decision: `<state>/profile.lock`, exclusive, held by the daemon and by the offline identity commands, released never unlinked; stale sockets removed only by the lock holder and only when they are the daemon's own; admin endpoint mutations over IPC are a runtime overlay, never persisted. |
