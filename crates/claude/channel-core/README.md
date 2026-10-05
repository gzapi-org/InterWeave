# channel-core

The Claude Code Channel bridge's pure half (plan §19 step 2):
- what a session's events become as `notifications/claude/channel`;
- the reply-token table;
- the tool surface's types and result wording.

It does no I/O. `apps/claude-channel` is the MCP server over the IPC client. It names nothing under `crates/transport/*` and no libp2p crate (§19 Rule), and its default features pull no CommonMark parser (`check_bridge_default_features.sh`).

**Current status:** active workspace member since Stage 16 batch 2.

## `reply_token`

Moved here from `crates/transport/runtime` at Stage 16 (plan §19 step 2): the bridge is its only caller, and the bridge may not name `crates/transport/*`.

A reply token is a **local routing handle, not a capability**. It records where a message came from so a reply can go back the same way, and confers nothing — current trust and endpoint policy apply to the reply as to any other send.

**Binding to the lease epoch is the whole mechanism.** A token stores the epoch it was minted under; after a reconnect the epoch is new, so every token from the previous session stops resolving. Without that, a reply issued after reconnect would be routed by a token whose local endpoint now belongs to a different session — delivering it as somebody else.

Unknown and expired are deliberately **one answer**: distinguishing them would tell a caller whether a token had ever existed. A broadcast token does not recreate a subscription — if the channel was left, replying fails `ChannelNotJoined` — and it carries no endpoint at all, because broadcast origin is PeerId-only.

The table bounds and expires tokens; it does not *generate* them. Unguessability needs a CSPRNG, which a pure module has no business owning.

