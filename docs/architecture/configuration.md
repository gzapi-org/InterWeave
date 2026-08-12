# Configuration architecture

## Separation

| Class | Example | Back up? | Secret? | Safe to delete? |
|---|---|---|---|---|
| normal config | listen addresses, providers, endpoint routes, channel subscriptions | yes | generally no | no |
| identity data | libp2p private key | yes, securely | **yes** | no, changes PeerId |
| mutable state | profile lock metadata, runtime state, endpoint leases | usually no | no | usually |
| peer cache | observed peers/addresses + bounded transport protocol observations | optional | no | **yes** |
| remote endpoint cache | short-lived advertised EndpointIds | no | no | **yes** |
| runtime IPC endpoint | socket/pipe | no | no | recreated |
| logs | structured diagnostics | policy-dependent | must be sanitized | yes |

## Config schema version

Model B is represented by architecture config `schema_version: 2`. The repository has no production schema-v1 deployment obligation, so Phase 1 targets v2 directly rather than silently synthesizing endpoint routes from old all-client fan-out behavior.

## Endpoint configuration

Endpoint configuration is profile-level normal configuration, separate from runtime leases.

```text
endpoints:
  registration_policy: configured-only
  default_direct_endpoint: human?
  directory:
    enabled: true
    cache_ttl: 60s
    max_advertised: 16
  entries:
    - id: human
      enabled: true
      advertise: true
      allowed_client_kinds: [human-client]
      inbound: { policy: inherit-profile-trust }
      outbound: { policy: inherit-profile-trust }
```

Normative rules:

- endpoint IDs are unique and satisfy `EndpointId` grammar;
- at most 64 configured endpoints;
- `default_direct_endpoint`, when set, references an enabled entry;
- endpoint `static-subset` peer lists must be subsets of profile `trust.allowed_peers`;
- endpoint policy can narrow but never widen profile trust;
- number of enabled `advertise: true` endpoints cannot exceed directory maximum;
- client kind is a hygiene check, not authentication;
- ordinary data-plane IPC clients cannot create endpoint entries dynamically.

Endpoint leases are runtime-only and never written back as configuration.

## Endpoint reload

Safe reloads include:

- enable/disable endpoint;
- advertisement flag;
- endpoint narrowing ACLs;
- default direct endpoint;
- directory enable/TTL/advertised cap within hard ceilings.

If a reload disables an actively leased endpoint or makes the current client ineligible, the daemon revokes that lease and emits an endpoint-lease operational event. It does not silently move the client to another endpoint.

Changing default endpoint affects only future peer-only direct requests. In-flight requests retain the route resolved at admission.

## Provider configuration

Provider-specific configuration is namespaced under each tagged provider entry. The core parser dispatches to a provider schema selected by `type`; it does not flatten every provider field into a global object.

Use typed tagged enums in Rust for built-in providers. A provider schema can carry `config_version` only when an actual migration is needed.

### Unsupported enabled providers

The configuration layer distinguishes unknown, implemented, and known-but-unbuilt providers. Any explicitly `enabled: true` provider must be implemented by the active daemon build. Kademlia stays `enabled: false` by default and is a hard startup/config error when enabled on an unsupported build.

## Desired channel subscriptions

`channels.desired` keeps selected backend broadcast subscriptions/mesh state pre-warmed across local client disconnects. It is not an IPC join, EndpointId subscription, delivery queue, or replay store.

## Effective limits and capabilities

`transport.limits.max_payload_bytes` may lower the profile's 49,152-byte ceiling. Active value is returned by capabilities.

The IPC v2 JSON-body ceiling of 131,072 bytes is a protocol constant so maximum payload plus two 64-byte EndpointIds and other bounded metadata remains representable.

## General reload

Safe reloadable classes: supported provider config, rate/queue limits within ceilings, trust allowlist, endpoint config as above, diagnostics, desired channels.

Trust reload can close data-plane connections, affect endpoint ACL intersections, and emits `TrustPolicyChanged`. Invalid reload leaves previous good config active.

Restart-required: identity key path/rotation, profile IPC socket identity, and core listen transport changes when backend cannot apply atomically.

## Kademlia configuration rule

The Kademlia schema is fully defined but optional. `enabled: false` is shipped/default. A supporting build may start it only after explicit opt-in.

When enabled, `network_id` defines the private protocol namespace, `routing_peer_policy: data-plane-trusted` and `record_mode: disabled` remain fixed security invariants, and all documented cross-field/seed-source constraints are hard validation rules.

Endpoint IDs are never stored as Kademlia keys/provider records. Endpoint discovery uses the separate trust-gated endpoint-directory protocol.
