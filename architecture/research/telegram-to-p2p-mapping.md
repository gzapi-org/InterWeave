# Telegram-to-P2P mapping

| Telegram plugin concept | General pattern | P2P equivalent | Treatment |
|---|---|---|---|
| Telegram MCP server | Channel bridge | InterWeave Claude Code Channel bridge | Adopt, split transport out |
| `StdioServerTransport` | local Claude integration | stdio MCP | Adopt |
| `experimental['claude/channel']` | Channel capability | same capability | Adopt |
| Bot API polling | external transport runtime | local IPC client to daemon | Replace |
| incoming Telegram update | external inbound event | normalized daemon message event | Replace |
| `notifications/claude/channel` | Channel push | same notification | Adopt |
| message `content` | application payload | text/base64 representation | Adopt separation |
| `meta.chat_id` | route context | channel/source peer/reply token | Adapt |
| Telegram group | broadcast context | `ChannelId` / GossipSub topic | Replace |
| private DM | directed context | direct libp2p request-response | Replace |
| numeric Telegram user ID | transport sender identity | libp2p PeerId | Replace; no app-role claim |
| `gate()` | sender admission | `PeerTrustPolicy` + resource validation | Adopt principle |
| pairing/allowlist | trust establishment | static PeerId allowlist in v1 | Adapt |
| `/telegram:access` terminal-only mutation | privileged trust admin path | local CLI/config, never channel-triggered | Adopt principle |
| reply tool | outbound operation | `reply(reply_token, payload)` | Adapt |
| outbound group send | one-to-many send | `broadcast(channel, payload)` | Replace |
| bot token | external credential | persistent libp2p private key | Similar sensitivity, different semantics |
| state directory | instance-local state | profile directory | Adopt and strengthen isolation |
| PID/stale poller handling | process ownership | daemon profile lock/socket | Replace |
| Telegram network reachability | platform-provided | DiscoveryProvider + ConnectionManager | New P2P responsibility |
| platform TLS/API security | link security | Noise-authenticated libp2p connection | Replace |
| Telegram routing | platform service | connected topology / relay / dialing | New P2P responsibility |
| Telegram group delivery | platform fanout | GossipSub mesh | Replace |
| Telegram direct delivery | platform DM | direct stream protocol | Replace |
| no Bot API history | realtime limitation | explicitly no v1 offline mailbox | Similar outcome, explicit contract |
| react/edit tools | platform UX | none | Not applicable |
| attachment download | platform file API | bytes are just payload; large objects out of scope v1 | Not applicable as core |
| permission relay | authenticated human approval | deferred | Reject for v1 |
