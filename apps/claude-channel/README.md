# claude-channel

The Claude Code Channel bridge's executable (plan §19 step 3): a stdio MCP server, started by Claude Code, over the transport daemon's data socket. It is a composition root. `crates/claude/channel-core` holds the rules (notification conversion, reply tokens, the tool surface), and `crates/local/ipc-client` holds the connection. It names nothing under `crates/transport/*` or `crates/discovery/*` and no libp2p crate (`check_claude_layering.sh`), and no CommonMark parser by default (`check_bridge_default_features.sh`).

**Current status:** active workspace member since Stage 16 batch 3; the MCP layer is in, the session loop follows.
