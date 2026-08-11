# GossipSub broadcast design

## Topic mapping

Logical `ChannelId` is mapped to an internal topic key:

```text
sha256("claude-p2p-channel/topic/v1\0" || channel_id_ascii)
```

Wire topic representation is a lowercase/base32 or hex encoding chosen by the implementation. The hash prevents casual raw-topic disclosure but does not resist dictionary guessing of low-entropy channel names.

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

## Authenticity, trust, and validation results

Use signed GossipSub messages and strict cryptographic/protocol validation. Data-plane connections are trust-gated by ADR-0011/0012, but a trusted forwarding neighbor can still relay a message whose authenticated original publisher is not locally allowlisted.

Application validation therefore reports results according to ADR-0029:

- `Reject` for objectively malformed/cryptographically invalid protocol data;
- `Ignore` for structurally valid messages from a locally unauthorized original publisher;
- `Accept` only for structurally valid messages from a locally authorized original publisher.

An unauthorized source is not accepted-and-dropped later and is not labeled objectively invalid solely because local trust differs.

## Deduplication

GossipSub's own cache is retained. The normalized runtime adds a bounded v1 key:

```text
(mode=broadcast, source_peer, channel, message_id)
```

This matches the backend-neutral dedup contract and avoids accidental cross-mode/channel collisions.

## Publish result

Local publish acceptance is the only synchronous success claim. Zero mesh peers may still allow a local publish path depending on backend state; diagnostics must expose `mesh_peer_count=0` as degraded channel reachability rather than claiming delivery.

## Ordering and acknowledgements

Messages from a source or topic have no total/global ordering guarantee at the transport contract. The runtime does not generate per-recipient acknowledgements for GossipSub. Publish success means local publish acceptance only; remote peers may be offline, disconnected, unsubscribed, partitioned, overloaded, or may stop propagation under their local policy.
