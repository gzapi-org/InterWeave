# Network identity

## Ownership

One transport profile owns one persistent **Ed25519** libp2p private key and derived PeerId for the initial software identity backend. The key is not tied to Claude, a human client, or any one local endpoint.

Multiple local applications may intentionally share that PeerId through EndpointIds. EndpointId does not derive from the private key and does not become a second cryptographic principal.

## Storage

Conceptual platform paths:

```text
config:   $XDG_CONFIG_HOME/claude-p2p-channel/profiles/<profile>/config.yaml
identity: $XDG_DATA_HOME/claude-p2p-channel/profiles/<profile>/identity.key
state:    $XDG_STATE_HOME/claude-p2p-channel/profiles/<profile>/...
cache:    $XDG_CACHE_HOME/claude-p2p-channel/profiles/<profile>/peers.json
run:      $XDG_RUNTIME_DIR/claude-p2p-channel/<profile>.sock
```

Endpoint definitions live in normal profile config. Endpoint leases/presence are runtime-only and are not identity key state.

`identity.key` is written using the implementation's supported libp2p portable private-key representation with owner-only permissions. The architecture does not make an external Ed25519 library's in-memory key type part of the transport API.

## Generation

Initial software profile creation generates an Ed25519 secret using a local CSPRNG. Established profiles never silently regenerate a missing/corrupt key.

A profile may create a human backup of that exact key through `cp2p-ed25519-bip39-entropy-v1`; see `contracts/IDENTITY-RECOVERY.md`. Recovery is an offline identity-file operation, not daemon IPC.

## Recovery

The optional 24-word recovery phrase encodes the exact **32-byte Ed25519 secret seed** using BIP-39's 256-bit entropy/checksum/English-word mapping only. It does not use BIP-39 wallet PBKDF2 or a mnemonic passphrase.

Restoring those exact secret bytes must reproduce the same public key and PeerId. Recovery records should also preserve the public expected PeerId and fail closed on mismatch.

The phrase is private-key-equivalent and must never cross IPC, Channel events, transport messages, discovery, endpoint directory, logs, or normal configuration.

## Rotation/compromise

Existing rules remain: no silent regeneration for an established profile, explicit atomic rotation with PeerId/trust impact, and out-of-band revocation after compromise.

Rotating PeerId affects **all** local EndpointIds because they share the profile identity. Renaming/restarting an EndpointId does not rotate PeerId.

A recovery phrase remains bound to the old key. It does not migrate trust to a new PeerId after rotation. Phrase disclosure is treated exactly as private-key theft.

## Future identity backends

Hardware-backed/non-exportable keys are compatible with the identity abstraction but may intentionally have no mnemonic recovery. A future non-Ed25519 software key algorithm requires a new recovery-format identifier and explicit migration/rotation decision.

Future SLIP-0039 support may threshold-share the same 32-byte Ed25519 secret seed, preserving the same PeerId, but is not part of v1 implementation scope.

## Identity layers

```text
PeerId
  = authenticated network transport identity

EndpointId
  = route selector inside that PeerId

Human/application identity
  = higher-layer binding outside this transport
```

Neither `PeerId` alone nor `PeerId + EndpointId` proves a person's name, organization, repository role, Claude instance type, administrator privilege, or other application semantics.
