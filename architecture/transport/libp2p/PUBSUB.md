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

`BroadcastMessageV1` is a fixed-width binary frame, frozen as byte vectors
in [`fixtures/gossipsub/broadcast-message-v1-frame.json`](../../../fixtures/gossipsub/broadcast-message-v1-frame.json).
Like `DirectMessageV2` it is deliberately **not** modelled as JSON Schema:
it is a byte layout rather than a document, and cross-implementation
agreement on it belongs in byte vectors. The schema manifest records that
boundary under `not_modelled` rather than leaving it an apparent gap.

```text
BroadcastMessageV1 {
  version: u8,                   // 1
  message_id: 16 bytes,          // exactly 128 bits, APPLICATION identity
  sent_at_ms: u64,               // diagnostic only
  media_type_len: u8,            // 0 => absent
  media_type: bytes,             // 1..128 ASCII when present
  payload_len: u32,
  payload: bytes <= effective profile max_payload_bytes <= 49152,
}
```

All multi-byte integer fields are **big-endian**, matching the direct
frame, the IPC length prefix, and `DirectContentFingerprintV1` — the only
choice under which this repository agrees with itself about byte order.

The **version byte is carried in band**, which the direct frame does not
need: direct takes its version from the negotiated protocol name
`/interweave/direct/2.0.0`, while a GossipSub topic negotiates nothing, so
the envelope is the only place a broadcast reader can learn what it is
holding.

`media_type_len = 0` encodes **absence**. No empty media-type string
exists on the wire.

**The envelope carries no EndpointId**, per ADR-0030: two local endpoints
sharing one PeerId are intentionally indistinguishable as transport-level
broadcast originators.

**The envelope carries no ChannelId.** The receiver determines the logical
channel exclusively from the GossipSub topic the message arrived on, which
is total for every topic this node is subscribed to — a node cannot
receive on a topic it did not derive from a ChannelId it holds. Carrying
the channel twice would create a disagreement case, and a publisher able
to assert a channel other than the topic it published on, with no
authority to decide which one wins.

`sent_at_ms` is diagnostic only. It is not used for authorization, replay
suppression, ordering, freshness rejection, or deduplication in v1, and no
admission path may read it.

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

The stored content identity is `DirectContentFingerprintV1` (ADR-0019),
computed over the media type and payload exactly as for direct. The name
is historical: the function is defined over content alone, it is local
state that never crosses the wire, and a second domain-separated variant
would freeze a value no implementation interoperates on. The two modes
cannot alias because the KEY already differs by `mode`.

**A same-key/different-body conflict is a local delivery decision, never a
validation verdict.** The same authenticated publisher reusing one
`message_id` on one channel with different content is reported to the mesh
as `Accept` — the message is validly signed and its publisher is
authorized, which is the whole of what the validation report answers — and
is then refused local delivery by the ADR-0019 conflict rule, delivering
nothing under either body.

This is not the "accept then locally drop for an unauthorized source" that
ADR-0029 forbids: the drop is a duplicate-identity decision, not a trust
decision, and ADR-0029's own implementation order already places dedup
*after* the report. `Reject` would be wrong because conflict is detectable
only against *local* cache state — a peer that never saw the first body
cannot see a conflict, so one node's cache would be penalising an honest
relay for forwarding a message every other node considers valid.

## Publish result

Local publish acceptance is the only synchronous success claim. Zero mesh peers may still allow a local publish path depending on backend state; diagnostics must expose `mesh_peer_count=0` as degraded channel reachability rather than claiming delivery.

## Implementation defaults (non-normative)

Mesh tuning — `mesh_n` and its low/high bounds, heartbeat interval, gossip
history, fanout TTL — is **not** frozen here. It is an operational
performance decision rather than a protocol one, so an implementation
takes its backend's defaults and may retune them without a compatibility
or fixture change. What *is* frozen is above: the topic key, the message
identity function, the envelope, and the validation mapping.

One value is not tuning and must be set deliberately: the backend's
maximum transmit size must admit a full envelope, which is
`max_payload_bytes` plus the 158-byte fixed maximum of every other field
(`1 + 16 + 8 + 1 + 128 + 4`). A backend default larger than that would let
a peer send a frame the transport accepts and the envelope decoder must
then reject; sizing it exactly means an oversized frame is refused before
it is buffered.

The GossipSub duplicate cache and the runtime dedup TTL are **intentionally
different layers with different lifetimes** — a short mesh-level window
suppressing wire-level reflection, and the longer bounded application
window of ADR-0019. They are not two settings of one thing and must not be
"corrected" to match.

## Ordering and acknowledgements

Messages from a source or topic have no total/global ordering guarantee at the transport contract. The runtime does not generate per-recipient acknowledgements for GossipSub. Publish success means local publish acceptance only; remote peers may be offline, disconnected, unsubscribed, partitioned, overloaded, or may stop propagation under their local policy.
