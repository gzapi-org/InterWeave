# Separate profile-scoped transport daemon

**Status:** Accepted

## Context

Persistent PeerId, multiple Claude sessions, independent upgrades, crash isolation, and network reconnection are first-class requirements. Embedding would couple all of these to a session subprocess.

## Decision

Select Architecture B: Claude MCP Channel bridge connects over local IPC to a separate Rust transport daemon. The daemon owns identity/network lifecycle and can survive Claude Code restarts.

Android is the one deployment binding that differs. Per ADR-0041 the first-party Android app embeds the same Rust `TransportRuntime` inside a foreground-service host rather than launching a standalone daemon, because a desktop-style daemon is a poor Android lifecycle/process primitive. This is **not** a second networking architecture: PeerId ownership, trust, discovery, endpoints, Kademlia, and connectivity semantics are identical. Desktop, server, and Claude integrations use the external daemon.

## Alternatives considered

Embedded Swarm per Channel process; hybrid helper process that exits with each bridge; system-wide single daemon without profiles.

## Consequences

Adds IPC, service lifecycle, version negotiation, and local security work. In return, Claude restarts do not rotate identity or churn network topology.

## Security implications

Process isolation limits network parser failures from directly sharing the MCP process. Local IPC becomes a new attack surface and is owner-ACL protected.

## Operational implications

Daemon may run as user service or manually. Transport and plugin can update independently within negotiated compatibility windows.

## Implementation implications

Define IPC before implementation. Bridge contains no private key or Swarm. Daemon supports explicit graceful shutdown and profile locking, but ordinary `claude-channel` IPC clients are not authorized to invoke daemon shutdown; an administrative client/service manager is required.

## Revisit conditions

Revisit only if measured deployment burden of a daemon is unacceptable and requirements for identity/network continuity are relaxed.

## Amendments

Full notes: [`history/0015-amendments.md`](./history/0015-amendments.md).

| Date | Amendment | Effect |
|---|---|---|
| 2026-08-12 | Android embeds the runtime instead of running a daemon | Decision states the ADR-0041 foreground-service binding; networking semantics unchanged |
