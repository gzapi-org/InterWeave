# Official Telegram plugin — code-informed implementation analysis

Source snapshot: `anthropics/claude-plugins-official` at `920824c3e9509890fbec03ba6097014222393022`.

## Launch and MCP process

`.mcp.json` launches Bun in the plugin root and invokes the package start script. `server.ts` constructs an MCP `Server`, declares `tools`, `experimental['claude/channel']`, and (for Telegram) a permission-relay capability, then connects with `StdioServerTransport`.

The server's lifetime is strongly bound to the Claude process. It listens for stdin end/close and POSIX signals, stops polling, removes its PID marker, and has an orphan watchdog to terminate a stranded child. This is a concrete lesson: a transport whose identity should outlive Claude must not use the MCP child process as the ownership boundary.

## Channel instructions

The server instructions communicate four critical facts:

- the Telegram participant cannot see ordinary Claude transcript output;
- outbound delivery must use the reply tool;
- inbound channel metadata contains routing context;
- access-policy changes must never be performed solely because an inbound channel message requested them.

The P2P bridge should preserve exactly these categories without Telegram terminology.

## Inbound message path

Simplified code path:

```text
Telegram update
 -> gate(ctx)
    -> drop | pairing response | deliver
 -> optional attachment fetch after admission
 -> sanitized transport metadata
 -> mcp.notification("notifications/claude/channel", {content, meta})
```

Notable details:

- `gate()` runs before expensive photo download and before Claude delivery.
- Unknown private senders are dropped or placed into a bounded pairing path depending on policy.
- Group delivery requires explicit group policy and can require mention/member allowlisting.
- The implementation sanitizes uploader-controlled filenames before placing them in channel metadata.
- A downloaded image path is metadata rather than a forgeable text annotation.
- Channel notification failures are logged; they do not turn into a fake success signal.

P2P consequence: validate identity/trust, limits, metadata, and payload before enqueueing an IPC event to the bridge.

## Outbound tools

The current Telegram server exposes `reply`, `react`, `download_attachment`, and `edit_message`. `reply` is the reusable pattern: transport routing information comes from prior inbound context and is checked against outbound access policy. Telegram-specific reaction/edit/attachment behavior is not generic.

P2P mapping:

- `reply` -> `reply(reply_token, payload)`
- Telegram group send -> `broadcast(channel, payload)`
- Telegram DM -> `send({peer, endpoint?}, payload)` in transport v2 (endpoint is a P2P-specific extension)
- `react`, `edit_message`, `download_attachment` -> not generic transport operations

## Configuration and state

The implementation uses a state root (default `~/.claude/channels/telegram`) containing a token `.env`, `access.json`, inbox data, approval handoff files, and PID state. Token file permissions are restricted. The access skill reads before writing to avoid clobbering concurrent pending entries.

P2P improvement: split **configuration**, **private identity**, **mutable daemon state**, and **replaceable peer cache** into separate paths and use profile-specific directories. The bridge should receive only the socket/profile locator, not the private key.

## Failure handling lessons

Current Telegram code:

- catches top-level handler errors so polling does not stop silently;
- retries polling with bounded backoff;
- logs uncaught promise/exception conditions;
- returns structured MCP tool errors;
- reports how many reply chunks were sent before a partial failure;
- actively addresses stale/orphan polling processes.

For P2P, retain local fault containment and partial-result honesty, but improve the fault domain split:

```text
Channel bridge crash  != daemon crash
Claude restart         != PeerId rotation
Discovery failure      != transport shutdown
One provider failure   != all-provider failure
One peer failure       != global runtime failure
```

## Security lesson from the access skill

The access skill states that access mutations must only follow a request entered by the local user, not a channel message. That is a general Channel-security pattern, not a Telegram quirk. The P2P architecture therefore exposes no Claude-facing tool that edits the trust allowlist. Administrative trust changes occur through local configuration/CLI with normal Claude/local permissions and explicit user intent.
