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

**Protocol era (SPIKE-001, Claude Code 2.1.285; A 2026-10-03).** Claude Code first sends `server/discover`, the probe for the 2026-07-28 protocol revision. The bridge answers it with JSON-RPC `-32601` (method not found) and is then sent `initialize` for revision `2025-11-25`, which it accepts; the host records `protocolEra: legacy`. The bridge stays in that era: the documentation says a channel server that negotiates the 2026-07-28 revision is not registered as a channel, and no run measured that revision (facts 1–3). `MCP_PROTOCOL_NEGOTIATION=legacy` on the host skips the probe; the bridge must not depend on it.

**Delivery condition (facts 4–6).** Channel notifications reached the model only in an interactive session started with `--dangerously-load-development-channels` and accepted on its warning screen; in `-p` sessions none reached the model at the three timings tried (other timings are not established), and a session without the flag logs the server as not in its channels list while the tools keep working. The bridge cannot detect the difference from its side and must not claim delivery; `status` reports what the bridge sent, never what the model received. The published-plugin path (`--channels`) is not established.

**Tag rendering (facts 9–15).** Each notification enters the conversation as a user-turn message `<channel source="<server name>" k="v" …>`, the body, `</channel>`: `source` is set by the host from the server name and comes first, the other `meta` keys follow in the order the server sent them (fact 10, as re-measured at 47bd7b3e: an unsorted `meta` renders unsorted) — the bridge emits them in `contracts/CHANNEL-EVENT.md`'s table order so a reader sees one order, which means serialising a struct whose fields are in that order, not a `serde_json::Map` (the workspace enables `preserve_order` nowhere — `crates/api/ipc-protocol/Cargo.toml` notes it — and a map would sort them); a key outside `^[a-zA-Z_][a-zA-Z0-9_]*$` is dropped and logged; values are XML-escaped by the host; **the body is not escaped** except a closing `</channel>`, so a forged opening tag, `<b>`, quotes and `&` pass through as written. The server's — the bridge's — `instructions` (INSTRUCTIONS.md) reach the conversation as an `mcp_instructions_delta` attachment holding the server's name and its text (fact 15). The rules this binds: every `meta` key the bridge emits matches the grammar (`contracts/CHANNEL-EVENT.md` names them, none called `source`); the body is `content` as the contract defines it — UTF-8 payload forwarded, non-UTF-8 as base64url — never host markup the bridge composed, and the bridge's own sanitisation of `meta` values stands beside the host's escaping, not instead of it.

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


## Connectivity status

The bridge may expose the daemon's backend-neutral `ConnectivitySummary` through its existing `status` tool. It does not expose AutoNAT probes, relay reservations, hole-punch commands, or infrastructure authorization as Claude tools. Those are daemon connectivity/administrative concerns.
