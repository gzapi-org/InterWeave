# Configuration architecture

## Separation

| Class | Example | Back up? | Secret? | Safe to delete? |
|---|---|---|---|---|
| normal config | listen addresses, providers, channel subscriptions | yes | generally no | no |
| identity data | libp2p private key | yes, securely | **yes** | no, changes PeerId |
| mutable state | profile lock metadata, runtime state | usually no | no | usually |
| peer cache | observed peers/addresses | optional | no | **yes** |
| runtime endpoint | socket/pipe | no | no | recreated |
| logs | structured diagnostics | policy-dependent | must be sanitized | yes |

## Provider configuration

Provider-specific configuration is namespaced under each tagged provider entry. The core config parser dispatches to a provider schema selected by `type`; it does not flatten every provider field into a global object.

Use typed tagged enums in Rust for built-in providers. This preserves validation and compile-time exhaustiveness. A provider schema can carry `config_version` only when an actual migration is needed; do not create gratuitous version numbers.

### Unsupported enabled providers

The configuration layer distinguishes:

- unknown provider type;
- known and implemented provider type;
- known/reserved but not implemented in this build.

A provider explicitly configured `enabled: true` must be implemented by the active daemon build. If `kademlia` is known by the schema but absent from the minimum-v1 build, enabling it is a **hard validation/startup failure**. `enabled: false` may remain in config for forward-compatible rollout. The daemon never silently ignores an explicitly enabled unsupported provider.

## Effective limits and capabilities

`transport.limits.max_payload_bytes` may lower the profile's payload limit from the v1 hard ceiling of 49,152 bytes. The active value is returned by `TransportCapabilities.max_payload_bytes` so the bridge/consumers can enforce the same limit before IPC/network dispatch.

The v1 IPC JSON-body ceiling of 131,072 bytes is a protocol constant rather than operator configuration because it must preserve the max-payload representation invariant.

## Reload

Safe reloadable classes: provider enable/disable/config **when supported by the active build**, rate/queue limits within hard ceilings, trust allowlist, diagnostics level, desired channels.

Trust reload can close now-unauthorized data-plane connections and emits `TrustPolicyChanged`. Invalid reload leaves the previous good configuration active and reports diagnostics.

Restart-required classes: identity key path/rotation, IPC endpoint, core listen transport changes if backend cannot apply atomically.
