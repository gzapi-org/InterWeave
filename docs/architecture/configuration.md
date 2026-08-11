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

## Reload

Safe reloadable classes: provider enable/disable/config, rate/queue limits within hard ceilings, trust allowlist, diagnostics level, desired channels.

Restart-required classes: identity key path/rotation, IPC endpoint, core listen transport changes if backend cannot apply atomically.

Invalid reload leaves the previous good configuration active and reports diagnostics.
