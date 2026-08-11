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
payload = opaque bytes <= 48 KiB
content_type? = bounded string
```

Do not rely on `sent_at` for ordering or authorization.

## Authenticity and trust

Use signed GossipSub messages and strict validation. Cryptographic source association enables PeerTrustPolicy decisions but does not itself establish trust.

## Deduplication

GossipSub's own cache is retained. The normalized runtime adds a bounded `(source_peer, message_id)` TTL cache so duplicate behavior is backend-independent at the local consumer boundary.

## Publish result

Local publish acceptance is the only synchronous success claim. Zero mesh peers may still allow a local publish path depending on backend state; diagnostics must expose `mesh_peer_count=0` as degraded channel reachability rather than claiming delivery.
