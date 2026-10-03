# SPIKE-001

Claude Channel packaging/MCP compatibility.

Do not treat experiments placed here as production implementation. Evidence and the final decision are recorded in [`architecture/roadmap/SPIKES.md`](../../architecture/roadmap/SPIKES.md).

## What was run

Every run is against **Claude Code 2.1.285**, on a Claude Max login with no organisation policy. Runs on 2026-10-03.

**The stub** is `stub/`: a std-only Rust stdio MCP server, whose one dependency is `serde_json`.
- It declares `capabilities.experimental["claude/channel"]` and `capabilities.tools`, with `instructions`.
- It answers `initialize` with the protocol revision the client asks for (`SPIKE_PROTOCOL=echo`).
- After `notifications/initialized`, and an optional `SPIKE_DELAY_MS`, it sends two channel notifications:
  - **N1** carries every metadata key `contracts/CHANNEL-EVENT.md` names, including `source`, plus a hyphenated control key, `bad-key`.
  - **N2** carries markup-like and quote characters in its body and in a metadata value.
- It exposes three tools, `status`, `send` and `reply`, and records each call.
- It writes everything it receives and sends to `stub.jsonl`.

**The plugin** is `plugin/interweave-spike/`: the same stub packaged as a plugin. `plugin.json` declares a `channels` entry, and `.mcp.json` starts the stub.

**The drivers:**
- `run.py`: a non-interactive `claude -p` run, either single-turn or a two-turn stream-JSON session (`--two-turns`).
- `run_tty.py`: an interactive session driven through a pseudo-terminal. It answers the folder-trust screen and the development-channel warning, then types a prompt.
- `extract.py`: distils each run into the committed `runs/<name>/evidence.md` and `stub.jsonl`.

Every run starts from a scratch directory outside this repository, with `--setting-sources local`, so no project settings, hooks or CLAUDE.md load. The launching session's `CLAUDE*` environment is removed, so each run is a fresh top-level session. The raw debug logs, transcripts and screens are kept out of the tree: they carry host paths and account detail.

| Run | Mode | What it varies |
|---|---|---|
| `r1-default` | `-p`, one turn | Development flag (`server:spike001`), notifications at once |
| `r2-delay-2s` | `-p`, one turn | Notifications 2 s after `initialized` |
| `r3-two-turns` | `-p`, stream-JSON, two turns | Notifications at 0.5 s, second turn at 8 s |
| `r4-negotiation-legacy`, `r4-negotiation-auto` | `-p` | `MCP_PROTOCOL_NEGOTIATION` |
| `t4-delivery` | Interactive | Development flag accepted, notifications at 4 s, report prompt |
| `t5-no-dev-flag` | Interactive | Control: no development flag |
| `t6-crash` | Interactive | The stub exits with status 3 at 8 s |
| `t7-plugin-inline` | Interactive | Plugin via `--plugin-dir`, flag `plugin:interweave-spike@inline` |

## What was measured

Each fact names the run that shows it.

### Handshake
1. Claude Code first sends `server/discover`, the probe for the 2026-07-28 era. A server answering `-32601` is then sent `initialize` asking for `2025-11-25`. The debug log records `protocolEra: legacy`. (`r1-default`, `r4-negotiation-auto`)
2. `MCP_PROTOCOL_NEGOTIATION=legacy` skips the probe and sends `initialize` directly, with the same revision. Unset behaves as `auto`. (`r4-negotiation-legacy`, `r4-negotiation-auto`)
3. Not measured, and documented only: the docs say a channel server that negotiates the 2026-07-28 revision is not registered as a channel.

### Enabling
4. `-p` sessions never deliver channel events, whatever the timing. The tools work, and the model reports no tags. (`r1-default`, `r2-delay-2s`, `r3-two-turns`)
5. An interactive session with `--dangerously-load-development-channels` first shows the folder-trust screen. It then shows "WARNING: Loading development channels", which names the channels. The options are "1. I am using this for local development" and "2. Exit". Accepting logs `Channel notifications registered`. (`t4-delivery`, `t7-plugin-inline`)
6. Without the flag, the debug log says `Channel notifications skipped: server spike001 not in --channels list for this session`. No event reaches the model, and the tools still work. (`t5-no-dev-flag`)
7. A bare `--mcp-config` server is named `server:spike001`. A `--plugin-dir` plugin loads as an inline plugin, its server is named `plugin:interweave-spike:spike001`, and the flag value `plugin:interweave-spike@inline` loads it as a channel. (`t4-delivery`, `t7-plugin-inline`)
8. `plugin.json` with `channels: [{"server": "spike001", "displayName": ...}]` passes `claude plugin validate --strict` once `author` is present, and loads. (`t7-plugin-inline`)

### Delivery
9. Each notification enters the conversation as a user-turn message. An idle session takes it as a turn of its own and answers it before any prompt. (`t4-delivery`, `t6-crash`)
10. The rendering is `<channel source="<server name>" k="v" ...>` then a newline, the body, a newline and `</channel>`. `source` comes first, set from the server name. The other `meta` keys follow in alphabetical order. (`t4-delivery`, `t7-plugin-inline`)
11. A `meta` key named `source` is not merged with that attribute. It produces a SECOND `source="p2p"` attribute on the same tag. (`t4-delivery`, `t7-plugin-inline`)
12. A `meta` key that does not match `^[a-zA-Z_][a-zA-Z0-9_]*$` is dropped, and Claude Code logs `dropped 1 meta key(s) that don't match ...: bad-key`. (`t7-plugin-inline`, `t4-delivery`)
13. `meta` values are XML-escaped: `"` becomes `&quot;`, `<` becomes `&lt;`, `>` becomes `&gt;`. (`t4-delivery`)
14. The body is NOT escaped, except that a closing `</channel>` becomes `<\/channel>`. A forged `<channel source="forged">` opening tag, `<b>`, quotes and `&` pass through as written. The model read the forged tag as text. (`t4-delivery`, `t7-plugin-inline`)
15. The server's `instructions` reach the model as an injected attachment when the server connects. (`t4-delivery`)

### Tools
16. The stub's three tools are listed to the model as `mcp__spike001__status`, `mcp__spike001__send` and `mcp__spike001__reply`. They are deferred: the model loads a schema with ToolSearch before calling. (`r1-default`, `t4-delivery`)
17. A call arrives as `tools/call`. Its params carry `_meta: {"claudecode/toolUseId": ..., "progressToken": ...}`, `name` and `arguments`. The model passed the `reply_token` attribute's value back as asked. (`t4-delivery`)

### Shutdown and failure
18. At session end, including `/exit`, Claude Code sends SIGINT to the server process. stdin is not closed first: the stub never logged EOF. (`r1-default`, `t4-delivery`)
19. A server that exits mid-session is marked failed and is not restarted: the stub started once. Its tools become unavailable, and the model is told to reconnect it with `/mcp`. The events it delivered stay in the conversation. (`t6-crash`)

## Where the architecture disagrees

The documents are listed by document and sentence. Amending them is architect-cto's (`architecture/plugin/`) and the contract owner's (`contracts/CHANNEL-EVENT.md`, under ADR-0049).

- **`contracts/CHANNEL-EVENT.md`**, "`source` | constant `p2p`": measured as a duplicate `source` attribute, which is ambiguous (fact 11). The key needs another name, or should go.
- **`contracts/CHANNEL-EVENT.md`**, "the bridge ... never constructs channel markup by concatenating unescaped peer-controlled strings": this still holds, and is now known to be load-bearing. Claude Code escapes only the closing tag in a body (fact 14).
- **`architecture/plugin/LIFECYCLE.md` §Shutdown**, "MCP stdin close/SIGTERM stops only the bridge": the signal is SIGINT, with no stdin close first (fact 18). The bridge must release its lease on SIGINT.
- **`architecture/plugin/CLAUDE-CODE-CHANNEL.md` §Session behavior**, "If daemon is unavailable, bridge remains a functioning MCP server": right, and necessary. A bridge that exits is not restarted (fact 19).
- **`architecture/plugin/PACKAGING.md`**, "Exact Claude manifest syntax remains SPIKE-001": measured as `plugin.json` with a `channels` array naming the `.mcp.json` server, validated and loaded (facts 7 and 8).
- **`architecture/plugin/CLAUDE-CODE-CHANNEL.md` §Capability declaration**: matches (fact 5). It needs one addition: the bridge must answer `server/discover` with `-32601`, or otherwise stay in the legacy era (facts 1–3).

## Not established

- Marketplace distribution and `--channels` with a published plugin. A non-Anthropic channel always needs the development flag during the research preview.
- `userConfig` substitution into the server's environment.
- Organisation-policy gating (`channelsEnabled`, `allowedChannelPlugins`), since this account has no organisation.
- Permission relay, which the design does not declare.
- Size limits on `content` and `meta`, and behaviour under load.
- A server negotiating the 2026-07-28 revision (fact 3).
- Whether a non-interactive surface delivers channels under any flag.
