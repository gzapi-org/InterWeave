# Claude Code Channel bridge

## Responsibility

The bridge is a small Claude-specific adapter. It is not the network runtime.

```text
Claude Code <--stdio MCP--> Channel bridge <--local IPC--> transport daemon
```

It owns:

- Channel capability declaration;
- MCP tools;
- Channel instructions;
- daemon connection/reconnect;
- conversion of normalized transport events to `notifications/claude/channel`;
- safe metadata formatting;
- short-lived reply-token mapping.

It does not own:

- PeerId private key;
- discovery providers;
- dialing/backoff;
- GossipSub;
- direct stream handling;
- trust configuration mutation;
- application payload semantics.

## Capability declaration

Target current Claude Channel pattern:

```text
capabilities.tools = {}
capabilities.experimental["claude/channel"] = {}
```

The bridge does not declare remote permission relay in v1.

## Inbound conversion

Daemon event:

```text
MessageReceived {
  message_id,
  source_peer,
  mode,
  channel?,
  payload,
  received_at,
}
```

becomes Channel notification with `content` and bounded string metadata as specified in `contracts/CHANNEL-EVENT.md`.

Transport admission already occurred in the daemon, but the bridge performs defense-in-depth schema/size checks before notifying Claude.

## Session behavior

If daemon is unavailable, bridge remains a functioning MCP server where possible, exposes `status`, and returns clear errors from network tools. It retries local IPC connection with bounded backoff. It never silently starts a second identity/daemon unless explicit launch policy says it is the owner of that profile service.
