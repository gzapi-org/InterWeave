# Packaging blueprint

No production plugin, daemon, or human client is created in this architecture repository.

## Future Claude plugin component

Conceptual contents:

```text
.claude-plugin/plugin.json
.mcp.json
skills/
bridge/
README.md
```

Bridge configuration must identify both transport profile and local EndpointId to claim over IPC v2. Exact Claude manifest syntax remains SPIKE-001.

## Future transport component

```text
bin/interweave-transportd
bin/interweave-transportctl
config schema v2/docs
optional platform service integration
```

Daemon and bridge may ship together but remain independently versioned behind IPC v2.

## Future human client

The human client is a separate application package that consumes IPC v2; it does not bundle a second libp2p identity/runtime by default.

Conceptual pieces:

```text
human UI/TUI/CLI
IPC v2 data-plane adapter
application-local contacts + ADR-0044 retention store (pending outbound, unread inbound, receiver-kept inbound)
separate admin/settings adapter or privileged connection
```

The data-plane session claims a configured EndpointId such as `human`. Admin settings require separate capabilities and explicit local action.

## Skills / local configuration helpers

Helpers may initialize profiles/endpoints, display status, edit non-secret config, and guide allowlist changes. Remote Channel/network content is never sufficient authority for trust or endpoint-administration mutation.
