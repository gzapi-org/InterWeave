# GossipSub architectural analysis

## Decision

GossipSub is the v1 broadcast mechanism and maps one logical `ChannelId` to one internal, domain-separated topic identifier.

## Why it fits broadcast

- one-to-many semantics match a logical Channel subscription;
- mesh forwarding avoids a single message broker;
- rust-libp2p integrates connection identity, validation, scoring hooks, and pub/sub in one event loop;
- duplicate suppression is intrinsic to GossipSub and can be reinforced at the normalized transport event boundary.

## What GossipSub does not provide

- peer discovery;
- trust or channel membership authorization;
- durable message storage/offline delivery;
- global ordering;
- exactly-once delivery;
- end-to-end confidentiality from forwarding peers;
- a directed-message primitive.

## v1 validation profile

Architecture target:

- message authenticity: signed;
- cryptographic/protocol validation: strict;
- explicit application validation reporting: `Reject` objectively invalid, `Ignore` valid-but-locally-unauthorized original source, `Accept` valid+authorized (ADR-0029);
- application payload cap: 48 KiB before transport envelope;
- topic string: hash of a versioned/domain-separated `ChannelId`, not the raw identifier;
- ordinary local data-plane connections are trust-gated;
- accepted messages then pass bounded runtime admission/rate/dedup limits before local consumers;
- PeerTrustPolicy determines whether an original publisher is eligible for propagation through this node and local Channel delivery.
- mesh duplicate identity is `GossipSubMessageIdV1 = SHA-256(domain || u16be(source_len) || source_peer_bytes || u64be(sequence_number))`; application-envelope IDs are not mesh keys.

### Message-ID rationale and target-library invariant

The target rust-libp2p GossipSub API exposes a configurable message-ID function; its documented default identifies messages by source PeerId plus sequence number, and its signed-message sequence number is a 64-bit big-endian value on the wire. This project freezes its own byte-exact source+sequence mapping rather than allowing application payload/envelope IDs to become mesh duplicate keys. Phase 2/SPIKE-002 must also pin the target library's receive ordering: cryptographically/protocol-invalid source/sequence claims must not create a valid-message duplicate-cache entry.

GossipSub scoring and advanced anti-Sybil tuning are implementation details to calibrate in a network spike. The architecture requires bounded peer counts, source rate limiting, and observable mesh health without exposing mesh internals to Claude.

## Confidentiality boundary

For `A -> B -> C`, Noise protects A-B and B-C separately. B can process the plaintext pub/sub payload. v1 therefore limits ordinary local GossipSub data-plane connectivity to trusted PeerIds and assumes every trusted forwarding peer is inside the plaintext confidentiality boundary. This is an explicit limitation, not a claim of end-to-end group secrecy.
