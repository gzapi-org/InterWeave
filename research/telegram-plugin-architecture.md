# Official Telegram plugin — architectural analysis

Source snapshot: Anthropic `claude-plugins-official` main `920824c3e9509890fbec03ba6097014222393022`.

## Architecture

```text
Claude Code
   |
   | stdio MCP
   v
Telegram MCP server (Bun / TypeScript)
   |
   | Bot API polling / outbound API
   v
Telegram
```

The implementation combines Channel bridge and external-transport client in one process. That is reasonable for Telegram because the Bot API token/poller is naturally coupled to one local server instance. P2P has a stronger requirement for persistent cryptographic identity and independent network lifecycle, so this repository adopts the Channel half of the pattern but splits transport ownership into a daemon.

## Patterns adopted

1. **stdio MCP bridge.** Claude Code launches the Channel-facing process.
2. **capability declaration.** `claude/channel` identifies the MCP server as a Channel.
3. **push, not poll.** Inbound external events become `notifications/claude/channel`.
4. **content/meta separation.** User text is separate from chat/user/message/timestamp routing metadata.
5. **pre-delivery sender gate.** Unknown/unauthorized senders are dropped or paired before content reaches Claude.
6. **reply routing metadata.** Inbound events preserve enough context for an outbound tool to address the correct external route.
7. **explicit instructions.** Claude is told that terminal transcript text is not remote delivery and that the reply tool must be used.
8. **trust configuration cannot be authorized by an inbound message.** The access skill explicitly refuses channel-triggered access changes.
9. **state outside plugin source.** Credentials/access/inbox are under a user state directory rather than committed plugin files.
10. **bounded platform limits and partial failure reporting.** Telegram chunks outbound text and reports partial sends rather than pretending atomicity.

## Patterns adapted

- Telegram `chat_id` becomes P2P `ChannelId`, transport `PeerId`, and an opaque `reply_token` depending on delivery mode.
- Bot-user allowlists become a transport `PeerTrustPolicy` based on PeerId for v1.
- Telegram external API client becomes local IPC to a persistent Rust transport daemon.
- Telegram's one-poller PID cleanup becomes unnecessary at the bridge layer; daemon instance ownership uses a profile lock/socket instead.

## Patterns rejected

- **Single process owns both Claude and transport lifecycle.** Rejected because a long-lived PeerId/network should survive Claude session restarts.
- **Platform-specific tools.** React/edit/file-download are Telegram-specific and not part of the generic P2P surface.
- **Pairing as a long-term policy.** P2P v1 uses explicit static trust entries; TOFU or signed membership are future policies.
- **Remote permission relay in v1.** Transport identity alone is insufficient authorization for local tool approvals.

## New P2P responsibilities

The Telegram backend does not require equivalents of P2P discovery aggregation, multiaddress management, peer dialing/backoff, GossipSub mesh operation, direct-stream protocol negotiation, persistent libp2p identity, NAT/relay strategy, Sybil/eclipse resistance, or discovery provenance. Those responsibilities live below the Channel boundary and are explicit in this architecture.
