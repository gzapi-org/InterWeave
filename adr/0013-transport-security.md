# Noise XX for libp2p TCP connection security

**Status:** Accepted

## Context

The initial backend needs encrypted authenticated peer connections and the prompt specifically requires evaluating Noise. The standard libp2p integration avoids custom cryptography.

## Decision

Use rust-libp2p Noise with the interoperable XX profile to authenticate PeerIds and encrypt TCP connections. Use Yamux above it for streams.

## Alternatives considered

Plain TCP; application-only encryption; bespoke TLS identity mapping; QUIC-only first transport.

## Consequences

Security is hop-by-hop and tightly integrated with libp2p identity. TCP remains the simplest initial transport while keeping future transports possible.

## Security implications

Noise does not authorize peers or hide GossipSub payloads from forwarding peers. Forward secrecy is limited to what the chosen Noise session construction provides; private-key compromise still requires rotation/revocation.

## Operational implications

Identity/key permission failures are fatal for the profile. Cryptographic handshake failures are peer-local diagnostics.

## Implementation implications

Use library defaults compatible with the libp2p spec; do not design a custom Noise handshake or key schedule.

## Revisit conditions

Revisit if transport support changes to QUIC/TLS or interoperability requirements demand a different libp2p security protocol.


Phase-9 relay paths do not change the authenticated application-peer boundary: the end peers still establish the project's Noise-secured libp2p session across the relayed path. Relay authorization is infrastructure authorization only (ADR-0036), not application trust. Relay operators remain availability/metadata observers and are not anonymity infrastructure.
