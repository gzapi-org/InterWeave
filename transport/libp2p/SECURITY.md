# libp2p security boundary

## Noise

v1 selects the rust-libp2p Noise integration (Noise XX interoperability profile) for TCP connection security.

Provides:

- encryption of each peer connection;
- authentication of the libp2p transport identity participating in the handshake;
- session key establishment with the forward-secrecy properties of the selected Noise handshake/session construction.

Does not provide:

- application authorization;
- channel membership semantics;
- organizational role identity;
- trust just because a PeerId was authenticated;
- end-to-end secrecy across GossipSub forwarding peers;
- protection after private-key theft against future impersonation until revocation/rotation reaches peers.

## Connection admission

Noise authentication establishes which PeerId is on the connection; it does not itself authorize that peer. For v1 ordinary data-plane connectivity:

```text
Noise-authenticated PeerId
 -> PeerTrustPolicy connection admission
 -> retain data-plane connection OR close as unauthorized
```

ConnectionManager does not dial an unauthorized PeerId and closes an unauthorized inbound connection before direct/GossipSub data-plane participation. Discovery may still retain candidate metadata independently.

## Message admission

Direct path:

```text
trusted Noise-authenticated connection
 -> direct protocol validation
 -> PeerTrustPolicy source check
 -> size/rate/dedup checks
 -> local delivery
```

GossipSub path:

```text
trusted direct neighbor
 -> signed message decode/source validation
 -> original-publisher PeerTrustPolicy check
 -> GossipSub validation result per ADR-0029
      Reject = objectively invalid
      Ignore = valid but locally unauthorized source
      Accept = valid + authorized source
 -> size/rate/dedup checks for accepted message
 -> local delivery
```

The immediate forwarding neighbor and original GossipSub publisher are distinct security facts and must not be conflated.

## Group encryption

Deferred in v1. Designing a secure group key lifecycle, membership change protocol, replay protection, and key rotation is out of scope for a generic transport architecture unless a concrete threat model requires it. The payload envelope retains room for a future end-to-end encrypted application payload; the transport will continue to treat ciphertext as opaque bytes.
