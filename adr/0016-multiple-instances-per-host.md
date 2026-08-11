# Profile-scoped identities with explicit daemon sharing

**Status:** Accepted

## Context

Several Claude instances on one host must coexist without accidental key sharing. At the same time, explicit connection reuse among intentionally shared sessions is valuable.

## Decision

Default to one network identity per named transport profile, not per Claude conversation and not one implicit host identity. Multiple Claude bridges may share a profile/daemon only by explicitly selecting the same profile/socket. Independent profiles have independent keys/state/sockets.

## Alternatives considered

PeerId per Claude process; one mandatory host-global PeerId; daemon multiplexes hidden per-client PeerIds inside one Swarm.

## Consequences

Identity semantics are clear: network sees the profile PeerId. Local endpoints sharing it are not distinguishable as separate network identities unless a higher-level payload protocol says so.

## Security implications

Explicit sharing prevents accidental privilege merging. Same-profile local clients share the trust/network authority of that profile, so profile socket access is sensitive.

## Operational implications

Operators can run `default`, `project-a`, `project-b` profiles. Resource usage scales by number of profiles rather than number of Claude sessions.

## Implementation implications

Profile path resolution is deterministic. Daemon lock prevents two owners. Local subscription refs are per IPC client.

## Revisit conditions

Revisit if a real requirement emerges for multiple cryptographic PeerIds inside one daemon process; that is a larger key/isolation design.
