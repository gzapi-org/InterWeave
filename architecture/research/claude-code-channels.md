# Claude Code Channels research

## Current integration contract

The current Claude Code Channels reference defines a Channel as a local MCP server spawned by Claude Code and connected over stdio. The server opts in through the experimental `claude/channel` capability and pushes inbound content with `notifications/claude/channel`. A two-way integration exposes ordinary MCP tools for replies or other outbound actions.

The Channel event has two logically different fields:

- `content`: external/application content to inject into Claude;
- `meta`: string-valued transport metadata which Claude renders as attributes on the channel event.

That distinction is a first-class design constraint for this project.

## Required pattern for this project

```text
Claude Code
   |
   | stdio MCP (Channel capability)
   v
InterWeave bridge
   |
   | versioned local IPC
   v
P2P transport daemon
```

The bridge, not the daemon, owns Claude-specific responsibilities:

- declare `claude/channel`;
- expose Claude-facing tools;
- translate daemon events into `notifications/claude/channel`;
- generate safe, minimal Channel instructions;
- convert transport payload representation to Channel `content` when necessary;
- maintain short-lived opaque `reply_token` mappings for outbound routing.

The daemon never sees Claude prompts or tool schema.

## Security rule: gate before injection

The Channel reference and official Telegram implementation both establish the same invariant: untrusted network reachability is not enough to inject content into Claude. Sender/trust/admission checks occur before Channel delivery.

P2P equivalent:

```text
network frame
  -> Noise authenticates transport PeerId
  -> PeerTrustPolicy admits/denies the transport identity
  -> message/framing/resource validation
  -> duplicate/rate checks
  -> local IPC event
  -> bridge validation
  -> notifications/claude/channel
```

No discovery provider can bypass this path.

## Process lifecycle implication

Claude Code starts/stops the Channel MCP bridge as a session subprocess. This project deliberately does **not** make that process own the network identity. The bridge may disappear while the daemon remains alive. While no bridge is connected, realtime network messages are not durably queued for later Claude delivery; they may be dropped with counters/diagnostics.

## Packaging implication

Current plugin documentation supports explicit `channels` declarations that bind a Channel to an MCP server. The current Telegram plugin source snapshot predates or does not use that field in its minimal manifest. The future implementation should follow the current documented contract after a packaging compatibility spike rather than mechanically copying one manifest.

## Permission relay

Claude Channels can optionally relay permission prompts through a channel when the remote responder is strongly authenticated. v1 of this architecture **does not opt into remote permission relay**. A transport PeerId proves a cryptographic network identity, not that the human/application behind it is authorized to approve local tool execution. Permission relay is a future security design, not a transport default.
