# Network identity

## Ownership

One transport profile owns one persistent libp2p private key and derived PeerId. The key is not tied to Claude, a human client, or any one local endpoint.

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

## Generation/rotation/compromise

Existing rules remain: local CSPRNG explicit initialization, no silent regeneration for established profile, explicit atomic rotation with PeerId/trust impact, and out-of-band revocation after compromise.

Rotating PeerId affects **all** local EndpointIds because they share the profile identity. Renaming/restarting an EndpointId does not rotate PeerId.

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
