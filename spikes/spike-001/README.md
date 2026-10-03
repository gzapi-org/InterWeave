# SPIKE-001

Claude Channel packaging/MCP compatibility.

Do not treat experiments placed here as production implementation. Evidence and the final decision must be recorded against [`architecture/roadmap/SPIKES.md`](../../architecture/roadmap/SPIKES.md).

## What was run

Every run is against **Claude Code 2.1.285**, on 2026-10-03, on a personal account with no organisation policy.
- **How the build is pinned:** the drivers run one explicit binary (`$SPIKE_CLAUDE`, here the installed 2.1.285 build), with `DISABLE_AUTOUPDATER=1`.
- **How each run is stamped:** `claude_version` is the version the session itself announced to the stub in `initialize`. A run fails if that disagrees with the binary's version.
- **Why:** an earlier campaign ran into a self-update from 2.1.285 to 2.1.288 midway, and its stamps were not trustworthy.

Each run's `evidence.md` names the model its session used.

**The stub** is `stub/`: a std-only Rust stdio MCP server. Its dependencies are `serde_json`, with `preserve_order`, and `signal-hook`, and its lock is committed.
- It declares `capabilities.experimental["claude/channel"]` and `capabilities.tools`, with `instructions`.
- It answers `initialize` with the protocol revision the client asks for.
- After `notifications/initialized`, and an optional `SPIKE_DELAY_MS`, it sends two channel notifications:
  - **N1** is a direct event. It carries every metadata key `contracts/CHANNEL-EVENT.md` names for a direct event, including `source`, written in a deliberately UNSORTED order. It also carries a hyphenated control key, `bad-key`.
  - **N2** is a broadcast event. It carries `channel`, and has markup-like and quote characters in its body and in a metadata value.
- It exposes three tools, `status`, `send` and `reply`.
- On SIGINT or SIGTERM it logs the signal and keeps reading stdin for 500 ms.
- It writes everything it receives and sends to its log.

**The plugin** is `plugin/interweave-spike/`: the same stub packaged as a plugin. `plugin.json` declares a `channels` entry, and `.mcp.json` starts the stub.

**The drivers:**
- `run.py` runs `claude -p` with stream-JSON output, either single-turn or with two turns (`--two-turns`).
- `run_tty.py` drives an interactive session through a pseudo-terminal. It answers the folder-trust screen and the development-channel warning, then types a prompt.
- `validate.py` runs `claude plugin validate --strict`.

Each run starts from a scratch directory outside this repository, with `--setting-sources local`. The run is given an allow-listed environment, so none of the launching session's tokens, markers or model overrides reach it. Its raw output is written outside the repository. The drivers share this setup through `spike_common.py`. `extract.py` distils the raw output into the committed `runs/<name>/evidence.md` and a redacted `stub.jsonl`.

All runs were produced and distilled by this tree as committed, at `1526799e`.

The interactive runs t6, t7 and v1 were re-run with a one-second pause after each expected screen. That pause is part of the `--keys` script, not of the code. In the first attempt the key presses arrived before the trust screen took input, so no stub started.

| Run | Mode | What it varies |
|---|---|---|
| `r1-default` | `-p`, one turn | Development flag (`server:spike001`), negotiation unset, notifications at once |
| `r2-delay-2s` | `-p`, one turn | Notifications 2 s after `initialized` |
| `r3-two-turns` | `-p`, two turns | Notifications at 0.5 s, second turn at 8 s |
| `r4-negotiation-legacy`, `r4-negotiation-auto` | `-p` | `MCP_PROTOCOL_NEGOTIATION` |
| `t4-delivery` | Interactive | Development flag accepted, notifications at 4 s, report prompt |
| `t5-no-dev-flag` | Interactive | Control: no development flag |
| `t6-crash` | Interactive | The stub exits with status 3 at 8 s |
| `t7-plugin-inline` | Interactive | Plugin via `--plugin-dir`, flag `plugin:interweave-spike@inline` |
| `v1-validate` | No session | `claude plugin validate --strict`, with and without `author` |

The interactive sessions also loaded an MCP connector configured on the account (a claude.ai server), which `--setting-sources local` does not exclude. It did not declare a channel. The evidence keeps only the stub's own server, so this sentence rests on the raw logs, which are not committed.

## What was measured

Each fact names the run that shows it.

### Handshake
1. Claude Code first sends `server/discover`, the 2026-07-28-era probe. A server that answers `-32601` is then sent `initialize`, asking for `2025-11-25`, and the debug log records `protocolEra: legacy`. This happened with negotiation unset (`r1-default`) and with `auto` (`r4-negotiation-auto`).
2. `MCP_PROTOCOL_NEGOTIATION=legacy` skips the probe and sends `initialize` directly, with the same revision. (`r4-negotiation-legacy`)
3. Not measured. The Claude Code MCP documentation (code.claude.com/docs/en/mcp, "Push messages with channels", read 2026-10-03) says that on the v2 MCP client runtime, a channel server that negotiates revision 2026-07-28 is not registered as a channel.

### Enabling
4. In `-p` sessions no channel event reached the model at the three timings tried: at once, at 2 s, and at 0.5 s with a second turn at 8 s. The stub's tools were listed. (`r1-default`, `r2-delay-2s`, `r3-two-turns`)
5. An interactive session with `--dangerously-load-development-channels` first shows the folder-trust screen. It then shows "WARNING: Loading development channels", which names the channels. The options are "1. I am using this for local development" and "2. Exit". Accepting logs `Channel notifications registered`. (`t4-delivery`, `t7-plugin-inline`)
6. Without the flag, the debug log says `Channel notifications skipped: server spike001 not in --channels list for this session`, and no event reaches the model. The stub's tools are listed. (`t5-no-dev-flag`)
7. A bare `--mcp-config` server is named `spike001`, and the flag value `server:spike001` loads it. A `--plugin-dir` plugin loads as an inline plugin, its server is named `plugin:interweave-spike:spike001`, and the flag value `plugin:interweave-spike@inline` loads it as a channel. (`t4-delivery`, `t7-plugin-inline`)
8. `plugin.json` with `channels: [{"server": "spike001", "displayName": ...}]`:
   - passes `claude plugin validate --strict` with `author` present;
   - fails `--strict` without `author`, on that one warning (`v1-validate`);
   - loads (`t7-plugin-inline`).

### Delivery
9. Each notification enters the conversation as a user-turn message. An idle session answers the events on its own before any prompt. (`t4-delivery`, `t6-crash`)
10. The rendering is `<channel source="<server name>" k="v" ...>`, then a newline, the body, a newline and `</channel>`. `source` comes first, set from the server name. The other `meta` keys follow **in the order the server sent them**: N1 was sent unsorted and rendered unsorted. (`t4-delivery`, `t7-plugin-inline`)
11. A `meta` key named `source` is not merged with that attribute. It produces a SECOND `source="p2p"` attribute on the same tag. (`t4-delivery`, `t7-plugin-inline`)
12. A `meta` key that does not match `^[a-zA-Z_][a-zA-Z0-9_]*$` is dropped, and Claude Code logs `dropped 1 meta key(s) that don't match ...: bad-key`. (`t4-delivery`, `t7-plugin-inline`)
13. `meta` values are XML-escaped: `"` becomes `&quot;`, `<` becomes `&lt;`, `>` becomes `&gt;`. (`t4-delivery`)
14. The body is NOT escaped, except that a closing `</channel>` becomes `<\/channel>`. A forged `<channel source="forged">` opening tag, `<b>`, quotes and `&` pass through as written. The model read the forged tag as text. (`t4-delivery`, `t7-plugin-inline`)
15. The server's `instructions` reach the conversation as an attachment of type `mcp_instructions_delta`, holding the server's name and its instructions text. (`t4-delivery`)

### Tools
16. The tool names the model sees depend on how the server is loaded:
    - A bare server's tools are `mcp__spike001__status`, `mcp__spike001__send` and `mcp__spike001__reply` (`r1-default`, `t4-delivery`).
    - A plugin's tools are `mcp__plugin_interweave-spike_spike001__status`, `…__send` and `…__reply`. These were listed, loaded and called, and the call reached the stub (`t7-plugin-inline`).

    The tools are deferred: the model says so (`r1-default`, `r3-two-turns`), and it loads a schema with ToolSearch before calling (`t4-delivery`, `t7-plugin-inline`).
17. A call arrives as `tools/call`. Its params carry `_meta` with a `claudecode/toolUseId` and a `progressToken`, plus `name` and `arguments`. The model passed the `reply_token` attribute's value back as asked. (`t4-delivery`)

### Shutdown and failure
18. At session end Claude Code sends SIGINT. If the process is still alive about 100 ms later, it sends SIGTERM ("SIGINT failed, sending SIGTERM", 8 runs). If it is still alive about 400 ms after that, it sends SIGKILL ("SIGTERM failed, sending SIGKILL", 6 runs).

    The stub catches both signals and keeps reading stdin for 500 ms, which is what kept it alive. Without a handler, the process ends at the SIGINT. In all seven runs that logged it, the 500 ms window completed, and stdin was never closed during it. (`r1-default`, `r2-delay-2s`, `r4-negotiation-auto`, `r4-negotiation-legacy`, `t4-delivery`, `t5-no-dev-flag`, `t7-plugin-inline`)
19. A server that exits mid-session is not restarted: its log shows one start. Afterwards its tools cannot be called: the model's ToolSearch finds none of them. The model says the server "failed to connect" this session, which repeats a Claude Code notice that is not itself in the committed evidence. The events delivered before the exit stay in the conversation. (`t6-crash`)

## Where the architecture disagrees

The documents are listed by document and sentence. Amending them is architect-cto's (`architecture/plugin/`) and the contract owner's (`contracts/CHANNEL-EVENT.md`, under ADR-0049).

- **`contracts/CHANNEL-EVENT.md`**, "`source` | constant `p2p`": measured as a duplicate `source` attribute (fact 11).
- **`contracts/CHANNEL-EVENT.md`**, "the bridge ... never constructs channel markup by concatenating unescaped peer-controlled strings": this still holds, and is load-bearing, because Claude Code escapes only the closing tag in a body (fact 14).
- **`architecture/plugin/LIFECYCLE.md` §Shutdown**, "MCP stdin close/SIGTERM stops only the bridge": the first signal is SIGINT, with no stdin close (fact 18). A server still alive after it gets SIGTERM at about 100 ms and SIGKILL at about 500 ms. A bridge cannot count on a graceful window, so releasing its lease must not depend on the bridge doing work after the first signal.
- **`architecture/plugin/CLAUDE-CODE-CHANNEL.md` §Session behavior**, "If daemon is unavailable, bridge remains a functioning MCP server": right, and necessary. A bridge that exits is not restarted (fact 19).
- **`architecture/plugin/PACKAGING.md`**, "Exact Claude manifest syntax remains SPIKE-001": measured as `plugin.json` with a `channels` array naming the `.mcp.json` server, validated and loaded (facts 7 and 8).
- **`architecture/plugin/CLAUDE-CODE-CHANNEL.md` §Capability declaration**: the declaration as written is what the stub declared, and it registered (facts 5 and 7). Answering `server/discover` with `-32601` kept the stub in the legacy era, where it registered (facts 1 and 2). Whether a 2026-07-28-era answer would also register is not measured; the documentation says it would not (fact 3).

## Not established

- Marketplace distribution and `--channels` with a published plugin. A non-Anthropic channel needs the development flag during the research preview.
- `userConfig` substitution into the server's environment.
- Organisation-policy gating (`channelsEnabled`, `allowedChannelPlugins`): this account has no organisation.
- Permission relay, which the design does not declare.
- Size limits on `content` and `meta`, and behaviour under load.
- A server negotiating the 2026-07-28 revision (fact 3).
- Whether any non-interactive surface delivers channels.
- Any other `-p` timing than the three tried (fact 4).
