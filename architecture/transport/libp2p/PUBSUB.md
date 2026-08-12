# GossipSub broadcast design

## Topic mapping

Logical `ChannelId` is mapped to an internal topic key:

```text
sha256("interweave/topic/v1\0" || channel_id_ascii)
```

Wire topic representation is a lowercase/base32 or hex encoding chosen by the implementation. The hash prevents casual raw-topic disclosure but does not resist dictionary guessing of low-entropy channel names.

Golden topic-key fixture:

```text
ChannelId = "general"
SHA-256   = 82695daad230a8a8ddb6e43aae1063e4f611ded53d710f48b2ed3d206211c3bc
```

## Envelope

Conceptual broadcast envelope:

```text
version = 1
message_id = 128-bit random identifier
sent_at = sender timestamp (diagnostic only)
media_type? = bounded string
payload = opaque bytes <= effective profile max_payload_bytes <= 49152
```

`sent_at` is diagnostic only. It is not used for authorization, replay suppression, ordering, freshness rejection, or deduplication in v1.

## Subscription and publish precondition

The calling local IPC client must hold an active join reference for the logical ChannelId before `broadcast`. Publishing without a caller-owned join fails locally with `ChannelNotJoined`; the runtime does not implicitly subscribe and does not borrow another local client's subscription reference.

Profile configuration `channels.desired` may keep the backend topic subscription/mesh warm even when zero IPC clients hold a join reference. That daemon-level subscription is **not** a client join: inbound messages with no joined local consumer are not buffered/replayed, and no bridge may publish merely because the profile desires the topic.

## Authenticity, trust, and validation results

Use signed GossipSub messages and strict cryptographic/protocol validation. Data-plane connections are trust-gated by ADR-0011/0012, but a trusted forwarding neighbor can still relay a message whose authenticated original publisher is not locally allowlisted.

Application validation therefore reports results according to ADR-0029:

- `Reject` for objectively malformed/cryptographically invalid protocol data;
- `Ignore` for structurally valid messages from a locally unauthorized original publisher;
- `Accept` only for structurally valid messages from a locally authorized original publisher.

An unauthorized source is not accepted-and-dropped later and is not labeled objectively invalid solely because local trust differs.

## Mesh-level message identity

The GossipSub `MessageIdFn` is frozen for valid v1 broadcasts and MUST bind the authenticated GossipSub source PeerId and the GossipSub wire sequence number. It MUST NOT depend on the application envelope `message_id`, and it is not legal to key GossipSub duplicate suppression on that 128-bit envelope field alone.

For a signed v1 GossipSub message:

```text
domain = UTF8("interweave/gossipsub-message-id/v1\0")
source = PeerId::to_bytes()          # canonical raw multihash bytes
sequence_number = GossipSub wire sequence number interpreted as u64
canonical = domain || u16be(len(source)) || source || u64be(sequence_number)
GossipSubMessageIdV1 = SHA-256(canonical)    # full 32 bytes
```

The message-ID function operates only on GossipSub transport metadata. It never parses the InterWeave broadcast envelope and therefore cannot make mesh duplicate suppression depend on application serialization. The source and sequence number are covered by the signed GossipSub message profile; strict protocol/signature validation remains mandatory. An implementation MUST verify during SPIKE-002/Phase 2 that the target rust-libp2p receive path rejects invalid signed-source/sequence messages before they can create a lasting valid-message duplicate-cache entry. If a future rust-libp2p version changes that ordering, the implementation must add an equivalent pre-cache authenticity gate or revisit this compatibility decision before release.

Golden fixture using the repository zero-seed PeerId and sequence number `0`:

```text
PeerId = 12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN
sequence_number = 0
GossipSubMessageIdV1 = 7f037dd538d9cccfb1949ca26b875c469173e6b248f1b68553ccaeb16bf9cf89
```

This mapping is network-compatibility behavior. Changing it requires a GossipSub compatibility/version decision, not a local cache tuning change. Two different authenticated source PeerIds using the same application-envelope `message_id` MUST remain distinct at the GossipSub duplicate-cache layer and both reach normal validation/runtime admission. Reuse of an envelope ID by the same source is still governed independently by the runtime dedup contract after GossipSub admission.

## Deduplication

GossipSub's own cache is retained with the frozen source+wire-sequence function above. The normalized runtime adds a bounded v1 key:

```text
(mode=broadcast, source_peer, channel, message_id)
```

This matches the backend-neutral dedup contract and avoids accidental cross-mode/channel collisions.

## Publish result

Local publish acceptance is the only synchronous success claim. Zero mesh peers may still allow a local publish path depending on backend state; diagnostics must expose `mesh_peer_count=0` as degraded channel reachability rather than claiming delivery.

## Ordering and acknowledgements

Messages from a source or topic have no total/global ordering guarantee at the transport contract. The runtime does not generate per-recipient acknowledgements for GossipSub. Publish success means local publish acceptance only; remote peers may be offline, disconnected, unsubscribed, partitioned, overloaded, or may stop propagation under their local policy.
