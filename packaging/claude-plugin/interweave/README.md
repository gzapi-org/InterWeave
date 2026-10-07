# InterWeave Claude Code plugin

The plugin that starts the Channel bridge, `apps/claude-channel`, inside Claude Code (`architecture/plugin/PACKAGING.md`, plan §19 step 3).

- `.claude-plugin/plugin.json` declares one channel, whose `server` is the `.mcp.json` key that starts the bridge. With `author` present it passes `claude plugin validate --strict` (SPIKE-001 fact 8).
- `.mcp.json` starts `bin/claude-channel` with the plugin's routing configuration: the transport profile's name and the local endpoint the bridge claims. The values here are the conventional `default` and `claude`. Set them to the profile and endpoint the daemon is configured with. It also passes `--delivery push`, which stays: Claude Code receives by the channel notification, and the bridge requires the mode. The bridge never chooses another endpoint on a conflict (`plugin/LIFECYCLE.md` §Endpoint conflict).
- `bin/claude-channel` is the built binary (`cargo build --release -p interweave-claude-channel`), copied in at packaging time and not committed.

Loaded with `--plugin-dir`, the plugin is inline: its server is named `plugin:interweave:interweave` and the channel flag value is `plugin:interweave@inline`. Channel events reach the model only in an interactive session started with `--dangerously-load-development-channels plugin:interweave@inline` and accepted on its warning screen (SPIKE-001 facts 4-7). Marketplace distribution and `--channels` are not established; they are measured before release (plan §22).
