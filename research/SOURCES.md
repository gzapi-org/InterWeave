# Research sources

Snapshot date: **2026-08-12**. Primary sources are preferred. URLs are recorded for implementation-time revalidation; unstable details must be checked again before coding.

## Claude Code / Anthropic

- Claude Code Channels reference: https://code.claude.com/docs/en/channels-reference
- Claude Code Channels user guide: https://code.claude.com/docs/en/channels
- Claude Code plugin reference: https://code.claude.com/docs/en/plugins-reference
- Claude Code plugin creation guide: https://code.claude.com/docs/en/plugins
- Claude Code MCP guide: https://code.claude.com/docs/en/mcp
- Official plugin repository: https://github.com/anthropics/claude-plugins-official
- Inspected repository head: `920824c3e9509890fbec03ba6097014222393022` (2026-08-10)
- Telegram files inspected at that snapshot:
  - `external_plugins/telegram/README.md` blob `b8702148aff8bd690398e413dd590206092d4049`
  - `external_plugins/telegram/ACCESS.md` blob `f762daf561baba3df6406e5bbe3038843810a7c2`
  - `external_plugins/telegram/server.ts` blob `23a21b06da426f6cfc4b86d24818128e09fed6ce`
  - `external_plugins/telegram/package.json` blob `bdbbea6bcf6174beb79a624b4baa8fc7eedc18d0`
  - `external_plugins/telegram/.mcp.json` blob `cf7195be355a449610d8153bfae6c4c394403c38`
  - `external_plugins/telegram/.claude-plugin/plugin.json` blob `e1edd215a64c503d01b74922acf78a1bce72f1fb`
  - `external_plugins/telegram/skills/access/SKILL.md` blob `5f112cfe0297263f1c97d3a0215e0217528f3483`
  - `external_plugins/telegram/skills/configure/SKILL.md` blob `31ad2f3a9affebe51e64eebc6fd3cfad83bf5ddb`

### Observed documentation/source skew

The current Claude plugin reference documents a manifest `channels` declaration bound to an MCP server. The inspected Telegram `plugin.json` is intentionally minimal and does not itself contain that field. This repository therefore treats **current Claude documentation as the target packaging contract** and the Telegram implementation as the primary behavioral reference. `SPIKE-001` must validate the exact packaging accepted by the Claude Code version targeted for implementation.

## MCP

- Current release announcement (2026-07-28): https://blog.modelcontextprotocol.io/posts/2026-07-28/
- Transport reference / draft index: https://modelcontextprotocol.io/specification/draft/basic/transports

MCP changed substantially in 2026. The Claude Channel reference remains authoritative for the Channel extension and stdio subprocess shape; the bridge implementation must use the SDK/spec version actually supported by the target Claude Code release.

## libp2p / rust-libp2p

- libp2p specifications: https://github.com/libp2p/specs
- rust-libp2p repository: https://github.com/libp2p/rust-libp2p
- rust-libp2p docs: https://docs.rs/libp2p/latest/libp2p/
- request-response: https://docs.rs/libp2p/latest/libp2p/request_response/
- request-response ProtocolSupport: https://docs.rs/libp2p/latest/libp2p/request_response/enum.ProtocolSupport.html
- GossipSub: https://docs.rs/libp2p/latest/libp2p/gossipsub/
- mDNS: https://docs.rs/libp2p/latest/libp2p/mdns/
- Kademlia: https://docs.rs/libp2p/latest/libp2p/kad/
- Kademlia DHT specification: https://github.com/libp2p/specs/blob/master/kad-dht/README.md
- Kademlia Behaviour: https://docs.rs/libp2p/latest/libp2p/kad/struct.Behaviour.html
- Kademlia Config: https://docs.rs/libp2p/latest/libp2p/kad/struct.Config.html
- Kademlia Event: https://docs.rs/libp2p/latest/libp2p/kad/enum.Event.html
- Kademlia BucketInserts: https://docs.rs/libp2p/latest/libp2p/kad/enum.BucketInserts.html
- Kademlia StoreInserts: https://docs.rs/libp2p/latest/libp2p/kad/enum.StoreInserts.html
- Kademlia PeerInfo: https://docs.rs/libp2p/latest/libp2p/kad/struct.PeerInfo.html
- Swarm NetworkBehaviour: https://docs.rs/libp2p/latest/libp2p/swarm/trait.NetworkBehaviour.html
- Swarm ToSwarm: https://docs.rs/libp2p/latest/libp2p/swarm/enum.ToSwarm.html
- Identify Info: https://docs.rs/libp2p/latest/libp2p/identify/struct.Info.html
- Noise: https://docs.rs/libp2p/latest/libp2p/noise/
- Identify: https://docs.rs/libp2p/latest/libp2p/identify/
- AutoNAT: https://docs.rs/libp2p/latest/libp2p/autonat/
- Relay: https://docs.rs/libp2p/latest/libp2p/relay/
- DCUtR: https://docs.rs/libp2p/latest/libp2p/dcutr/

The 2026-08 research snapshot observed the `libp2p` crate documentation at the 0.56 line. The implementation should not hard-pin to that research version without revalidation.

## Alternative transport primary sources

- Nostr NIP-01: https://github.com/nostr-protocol/nips/blob/master/01.md
- Matrix specification: https://spec.matrix.org/
- NATS Core concepts: https://docs.nats.io/nats-concepts/core-nats
- Redis Pub/Sub: https://redis.io/docs/latest/develop/interact/pubsub/
- MQTT v5 standard landing page: https://docs.oasis-open.org/mqtt/mqtt/v5.0/mqtt-v5.0.html

## Source interpretation rule

This repository records architectural conclusions, not copied implementation. Source-specific behavior is paraphrased. Where behavior is version-sensitive, the document labels it as a research snapshot or creates a spike/revisit condition.
