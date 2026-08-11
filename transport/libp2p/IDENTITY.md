# Network identity

## Ownership

One transport profile owns one persistent libp2p private key and derived PeerId. The key is not tied to a Claude conversation and is never sent through MCP/IPC/network messages.

## Storage

Conceptual platform paths:

```text
config:   $XDG_CONFIG_HOME/claude-p2p-channel/profiles/<profile>/config.yaml
identity: $XDG_DATA_HOME/claude-p2p-channel/profiles/<profile>/identity.key
state:    $XDG_STATE_HOME/claude-p2p-channel/profiles/<profile>/...
cache:    $XDG_CACHE_HOME/claude-p2p-channel/profiles/<profile>/peers.json
run:      $XDG_RUNTIME_DIR/claude-p2p-channel/<profile>.sock
```

Platform equivalents apply on macOS/Windows. Identity file permissions must be owner-only; parent data directory must not be group/world writable.

## Generation

Generate locally from an OS CSPRNG on explicit profile initialization. A missing key on a previously initialized profile is an error, not permission to silently create a new identity.

## Rotation

Rotation is an explicit local administrative operation:

1. generate new key atomically;
2. show old/new PeerId and trust impact;
3. require explicit confirmation/approved maintenance path;
4. replace key atomically and increment identity epoch;
5. peers will see a new identity and existing allowlists will not automatically transfer.

No automatic trust continuity is claimed. A higher-level signed rotation certificate is a future extension.

## Compromise

Treat stolen key as transport identity compromise: stop/rotate identity, revoke old PeerId in peer allowlists, distribute new trust configuration through an out-of-band trusted path. The transport cannot prove that a new PeerId is the same application entity without a higher-level binding.

## Transport identity is not application identity

`PeerId` answers: "which libp2p cryptographic transport identity authenticated this connection/message?"

It does **not** answer:

- which person or organization controls the peer;
- which Claude instance/application role it represents;
- whether it owns a repository/project;
- whether it may approve permissions or administrative changes.

A higher-level application may bind a logical identity to a PeerId, but that binding is outside the generic transport and must not be inferred here.
