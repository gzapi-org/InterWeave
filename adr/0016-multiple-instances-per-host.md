# Profile-scoped identities with explicit daemon sharing

**Status:** Accepted

## Context

Several Claude instances on one host must coexist without accidental key sharing. At the same time, explicit connection reuse among intentionally shared sessions is valuable. If several bridges intentionally share one profile/PeerId, inbound direct traffic has no network-level field that identifies one local Claude process.

## Decision

Default to one network identity per named transport profile, not per Claude conversation and not one implicit host identity. Multiple Claude bridges may share a profile/daemon only by explicitly selecting the same profile/socket. Independent profiles have independent keys/state/sockets.

Local message fan-out is explicit:

- **broadcast:** only IPC clients that currently hold a local join reference for the ChannelId receive that broadcast event;
- **direct:** every currently connected local IPC client granted message-event delivery receives its own copy of an admitted direct `MessageReceived` event.

v1 does not elect one local direct-message consumer, hide an implicit primary bridge, or add a local endpoint identifier to the network protocol. Two Claude bridges sharing a profile may therefore both observe and reply to the same direct message. Finer local routing belongs to a future explicit endpoint/application protocol, not an accidental daemon heuristic.

## Alternatives considered

PeerId per Claude process; one mandatory host-global PeerId; daemon multiplexes hidden per-client PeerIds inside one Swarm; first-connected bridge wins direct traffic; round-robin direct delivery; require application endpoint IDs in the transport envelope.

## Consequences

Identity semantics are clear: the network sees the profile PeerId. Local endpoints sharing it are not distinguishable as separate network identities unless a higher-level payload protocol says so. Direct inbound duplicate delivery to multiple local clients is intentional and documented.

## Security implications

Explicit sharing prevents accidental privilege merging. Same-profile local clients share the ordinary transport/trust authority of that profile, so profile socket access is sensitive. Administrative IPC methods such as daemon shutdown are separately capability-scoped and are not granted to Channel clients.

A direct remote sender cannot select or authorize one same-profile Claude client through transport metadata in v1.

## Operational implications

Operators can run `default`, `project-a`, `project-b` profiles. Resource usage scales by number of profiles rather than number of Claude sessions. If duplicate direct handling is undesirable, use separate profiles/PeerIds or a higher-level application routing convention.

## Implementation implications

Profile path resolution is deterministic. Daemon lock prevents two owners. Local subscription refs are per IPC client. Direct event fan-out creates independent bounded queue entries and independent bridge-local reply tokens for each receiving client; one slow client does not block another.

## Revisit conditions

Revisit if a real requirement emerges for multiple cryptographic PeerIds inside one daemon process or network-addressable local endpoints. Either is a larger identity/routing design.
