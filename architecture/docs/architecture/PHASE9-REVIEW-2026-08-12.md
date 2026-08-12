# Mandatory Phase 9 reachability review — 2026-08-12

## Decision delta

Phase 9 is no longer conditional. ADR-0035 supersedes ADR-0024 and makes AutoNAT v2 client, Circuit Relay v2 client/reservation management, and DCUtR part of the standard v1 release. Relay-server and AutoNAT-server roles are supported but explicitly configured infrastructure roles.

ADR-0036 adds a connectivity-infrastructure-only PeerId class so a relay/probe server can be authorized for reachability control without becoming a GossipSub/direct/endpoint/Kademlia application peer.

## Required release properties

1. At least one authenticated direct or relayed path can carry normal transport protocols to an authorized application PeerId.
2. Private/unknown peers maintain redundant relay reservations by default.
3. Public peers keep one warm relay reservation by default for failover/network change.
4. AutoNAT-v2 evidence never grants trust and requires multiple distinct authorized observations before `VerifiedPublic`.
5. Relay loss triggers bounded reselection/backoff; it never changes PeerId or EndpointIds.
6. DCUtR failure keeps the relay path; success changes preference for **new** streams only after direct stability.
7. All direct, relay, AutoNAT, Kademlia, and DCUtR dials cross the root dial-admission gate with origin attribution.
8. Infrastructure-only PeerIds cannot exchange GossipSub/direct/endpoint-directory/Kademlia application data.
9. Relay reservation addresses are ephemeral and removed when the reservation expires/closes.
10. Relay/AutoNAT server resource use is bounded by explicit global/per-peer limits.

## SPIKE-004 closure conditions

SPIKE-004 is now verification/tuning, not feature selection. It must validate the selected rust-libp2p APIs, protocol-admission matrix, relay failover, direct/relay address advertisement, DCUtR race behavior, and dial-gate attribution under the target dependency version. Failure blocks the standard-v1 release or requires a new ADR; it does not silently demote Phase 9 to optional.
