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

## Admission pipeline

```text
Noise-authenticated PeerId
 -> transport protocol validation
 -> PeerTrustPolicy
 -> channel subscription/policy checks
 -> size/rate/dedup checks
 -> local delivery
```

## Group encryption

Deferred in v1. Designing a secure group key lifecycle, membership change protocol, replay protection, and key rotation is out of scope for a generic transport architecture unless a concrete threat model requires it. The payload envelope retains room for a future end-to-end encrypted application payload; the transport will continue to treat ciphertext as opaque bytes.
