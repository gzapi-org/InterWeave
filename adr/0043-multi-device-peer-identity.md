# Concurrent human devices use distinct transport PeerIds

**Status:** Accepted

## Context

A human may use desktop and Android simultaneously. ADR-0033 makes the transport identity recoverable, which could be misread as an account seed intended to clone one PeerId onto every device. Simultaneous libp2p nodes presenting the same PeerId create ambiguous routing/connection semantics and turn disaster recovery into unsafe identity duplication.

## Decision

Each concurrently active physical device/profile uses a distinct PeerId. The 24-word phrase restores/migrates one transport identity after loss/retirement; it is not multi-device account synchronization. A human client's local contact model may group several PeerId/EndpointId routes under one person, but that association is not transport-authenticated.

## Alternatives considered

Clone one PeerId across all devices; make EndpointId globally identify devices under one shared key; invent account identity inside transport; prohibit multi-device human use.

## Consequences

Desktop and Android can be simultaneously reachable without PeerId collision. Contacts can show multiple device routes. Cross-device history sync is not automatic in v1.

## Security implications

Compromise of one device key does not automatically clone every device's transport key. Local grouping/display names do not grant trust to newly added device PeerIds.

## Operational implications

Users must trust/add each device PeerId or use a future higher-level device-linking protocol. Recovery UX warns when importing an identity expected to still be active elsewhere.

## Implementation implications

Human application state supports multiple routes per contact. No transport change is required.

## Revisit conditions

Revisit only with a separately designed signed human/account/device identity protocol above transport.
