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
- validation mode: strict;
- application payload cap: 48 KiB before transport envelope;
- topic string: hash of a versioned/domain-separated `ChannelId`, not the raw identifier;
- runtime-level admission/rate limits occur before forwarding to local consumers;
- PeerTrustPolicy determines whether a source is eligible for local Channel delivery.

GossipSub scoring and advanced anti-Sybil tuning are implementation details to calibrate in a network spike. The architecture requires bounded peer counts, source rate limiting, and observable mesh health without exposing mesh internals to Claude.

## Confidentiality boundary

For `A -> B -> C`, Noise protects A-B and B-C separately. B can process the plaintext pub/sub payload. v1 therefore assumes every peer allowed to participate in a sensitive channel is an appropriately trusted data-plane peer. This is an explicit limitation, not a claim of end-to-end group secrecy.
