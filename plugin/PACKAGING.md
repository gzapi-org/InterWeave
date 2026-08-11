# Plugin packaging blueprint

No production plugin package is created in this architecture repository.

## Future Claude plugin component

Conceptual distribution contents:

```text
.claude-plugin/plugin.json   # identity + current Channel declaration
.mcp.json                    # MCP bridge launch command
skills/                      # optional local configure/status helpers
bridge/                      # packaged bridge runtime
README.md
```

The current Claude plugin reference documents a `channels` manifest field that binds a Channel to an MCP server. The inspected Telegram plugin source snapshot has a minimal manifest without that field. `SPIKE-001` resolves the exact target syntax against the Claude Code release used for implementation.

## Future Rust transport component

Conceptual distribution contents:

```text
bin/claude-p2p-transportd
bin/claude-p2p-transportctl
platform service integration (optional packaging layer)
config schema/docs
```

The daemon and bridge may ship in one installer/archive, but they remain separately versioned architectural components and communicate only through the local IPC contract.

## Skills

If configuration skills are included, they may:

- initialize a profile;
- display status;
- edit non-secret config with explicit local user intent;
- guide allowlist changes.

A skill that changes trust must enforce the same rule as Telegram's access skill: a remote Channel message is never sufficient authority to approve/revoke peers.
