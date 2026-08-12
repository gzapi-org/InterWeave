# Claude Code Channel bridge

## Responsibility

The bridge is a small Claude-specific adapter. It is not the network runtime.

```text
Claude Code <--stdio MCP--> Channel bridge <--local IPC v2 / EndpointId--> transport daemon
```

It owns:

- Channel capability declaration;
- MCP tools;
- Channel instructions;
- daemon connection/reconnect and non-administrative IPC capability negotiation;
- claim/release of one configured local EndpointId lease through its IPC connection;
- conversion of normalized transport events to `notifications/claude/channel`;
- safe metadata formatting;
- short-lived reply-token mapping.

It does not own:

- PeerId private key;
- endpoint configuration/ACL/default-route administration;
- discovery providers;
- dialing/backoff;
- GossipSub;
- direct stream handling;
- trust configuration mutation;
- application payload semantics;
- daemon administrative shutdown.

## Capability declaration

Target current Claude Channel pattern:

```text
capabilities.tools = {}
capabilities.experimental["claude/channel"] = {}
```

The bridge does not declare remote permission relay in the initial design.

## IPC endpoint claim

Bridge configuration supplies one EndpointId, for example `claude`. IPC v2 hello requests that endpoint. The daemon grants the claim only if it is configured, enabled, allowed for the client kind, and not already leased.

No random fallback EndpointId is generated on conflict. A second same-profile Claude bridge must use another configured endpoint (`claude.secondary`, etc.) if both need independent direct addressing.

## Inbound conversion

Daemon direct event:

```text
MessageReceived {
  message_id,
  source_peer,
  source_endpoint,
  destination_endpoint,
  mode: direct,
  payload,
  received_at,
}
```

or broadcast event:

```text
MessageReceived {
  message_id,
  source_peer,
  mode: broadcast,
  channel,
  payload,
  received_at,
}
```

becomes Channel notification with `content` and bounded string metadata per `contracts/CHANNEL-EVENT.md`.

Transport admission already occurred in the daemon, but the bridge performs defense-in-depth schema/size checks before notifying Claude. Generic `payload.media_type` maps to Channel metadata `content_type`.

## Direct reply routing

For direct inbound, bridge-local reply token binds the exact route:

```text
remote = source_peer/source_endpoint
local = destination_endpoint + current lease epoch
```

Reply never targets another local endpoint or remote default as fallback.

## Session behavior

If daemon is unavailable, bridge remains a functioning MCP server where possible, exposes `status`, and returns clear errors from network tools. It retries local IPC connection with bounded backoff.

On reconnect it performs a fresh endpoint claim and fresh channel joins. Endpoint lease loss means no direct-message replay and no stale reply-token recovery.

It never silently starts a second identity/daemon unless explicit launch policy says it owns that profile service.
