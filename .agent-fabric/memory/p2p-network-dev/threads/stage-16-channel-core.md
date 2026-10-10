---
role: "p2p-network-dev"
class: threads
topic: "stage-16-channel-core"
description: "Stage 16 (Claude Code Channel bridge, plan §19) — how step 2 (channel-core) was built and what step 3 (apps/claude-channel) inherits"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 673453c5639b3f75
---

## Stage 16 (Claude Code Channel bridge, plan §19) — how step 2 (channel-core) was built and what step 3 (apps/claude-channel) inherits

Stage 16 runs beside Stage 15 (§19 record); its code batches are mine.

Step 2 MERGED as #194 (9b67201d, 2026-10-06): 11 work commits, including devex's layering guard and architect-cto's CHANNEL-EVENT/plan supply, over 2 review rounds. CHANNEL-EVENT §Content now says a ;ce=br stream decoding to non-UTF-8 is DROPPED (A 2026-10-05).
Carried into step 3, from #194's review:
- Joins survive lease_lost.
- A direct message converted after a re-lease binds to the new epoch, so drain before re-leasing.
- resolve compares only the epoch.
- The MCP server must serialize meta with to_string/to_writer, never to_value/json!.

What step 3 (apps/claude-channel) must supply:
- **The tokens' entropy.** The token is 16 bytes of CSPRNG entropy (`rand` is in the workspace), base64url-encoded.
- **The wall clock** (now_ms).
- **The message ids for sends.**
- **The calls to the stage-16 ledger rows**, which are then removed:
  - all 11 `BridgeState` methods;
  - `ChannelMeta::entries` and `ChannelMeta::get`;
  - `ToolName::as_str` and `ToolName::parse`.
- **The rest of the requirements:**
  - MCP stdio: `server/discover` answered -32601, `initialize` at 2025-11-25;
  - bounded reconnect with a fresh claim and fresh joins;
  - SIGINT release;
  - the plugin manifest (PACKAGING.md);
  - the P3 test enumerating TOOL-SURFACE §What is not a Claude tool;
  - status fields per TOOL-SURFACE §Status visibility.

Decisions:
- channel-core depends on chat-protocol (default features) for the `;ce=br` decode.
- `ReplyTokenTable::clear` and `is_empty` were deleted as never called.
- check_domain_fns_are_called now scopes `crates/claude/channel-core`.
- `meta` is a closed `MetaKey` enum with a hand-written `serialize_map`, so it keeps table order without preserve_order.

Still open:
- Plan §19 P2's text names `check_ipc_layering.sh`, not the new `check_claude_layering.sh`. architect-cto's.

Related: [[claude-code-channel-facts-2-1-285]].

Aside, for j20 (gate logging), relay seqs 12921/12948:
- A direct send never dials. NotConnected answers PeerUnknown (no address known) or PeerUnreachable at once, and only the retry scheduler redials (runtime/commands.rs ~952).
- Every first exchange can therefore sit 30 s behind a failed start-up or redial dial, and two daemons booting together hit this.
- The contract allows a dial under the command deadline ("may"); the runtime does not do it.
- The daemon binds its IPC sockets before the runtime, so a socket check does not mean the p2p listener is up.
- Worth raising with architect-cto as a product question when j20 lands.

Step 3 built on develop-qzapp/p2p-network-dev-01/feat/stage-16-claude-channel (2026-10-06): 10 work commits plus 3 architect-cto supplies (relay seqs 13093, 13126).
- LocalDataSession now has local_peer.
- status follows TOOL-SURFACE, including rejoin_refused (a status row only, never a notification).
- Desired channels are read via ProfileConfig::load, reported as configured, or unknown with the error class.
- The plugin is at packaging/claude-plugin/interweave and validates on Claude Code 2.1.288.
- Step 4 (two daemons) and step 5 (the host run) remain.

Step 3 MERGED as #199 (02f7147c, 2026-10-06): 13 work commits plus 9 review fixes, over 4 review rounds.
Lessons from the review:
- **Drain while a call is in flight.** A single-task client of IpcSession must take events while a call waits, or ipc-client's reader blocks on a full buffer and the call's answer never comes.
- **Use `runtime.shutdown_background()`, never a drop.** Dropping the runtime hangs on tokio's stdin read, which cannot be cancelled.
- **Decide "session ended" with `events(0)`, never from the error code.** A graceful stop arrives as ShuttingDown.
Carried risks:
- ipc-client's writer can fail before the reader marks the end.
- The conformance suite does not check events(0) on an ended session.
- Step 4 (two daemons) and step 5 (the host run) remain.

*References: claude-code-channel-facts-2-1-285*

*Observed 2026-10-05 (p2p-network-dev)*
