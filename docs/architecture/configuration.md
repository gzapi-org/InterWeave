# Configuration architecture

## Separation

| Class | Example | Back up? | Secret? | Safe to delete? |
|---|---|---|---|---|
| normal config | listen addresses, providers, endpoint routes, channel subscriptions | yes | generally no | no |
| identity data | libp2p Ed25519 private key | yes, securely | **yes** | no, changes PeerId |
| offline recovery record | 24 words + expected PeerId | yes, offline | **yes** (words) | no if it is the only backup |
| mutable state | profile lock metadata, runtime state, endpoint leases | usually no | no | usually |
| peer cache | observed peers/addresses + bounded transport protocol observations | optional | no | **yes** |
| remote endpoint cache | short-lived advertised EndpointIds | no | no | **yes** |
| runtime IPC endpoint | socket/pipe | no | no | recreated |
| logs | structured diagnostics | policy-dependent | must be sanitized | yes |


## Identity algorithm and recovery

The initial software profile algorithm is fixed to `ed25519`. The key file remains identity data, not YAML secret material.

Optional recovery uses the offline `cp2p-ed25519-bip39-entropy-v1` record defined in `contracts/IDENTITY-RECOVERY.md`. The 24 words are never stored in this schema. `identity.key_file` is only a path override. Backup/restore requires daemon-offline exclusive identity access and therefore is not a hot-reload operation.

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

The configuration layer distinguishes unknown, implemented, and known-but-unbuilt providers. Any configured/defaulted `enabled: true` provider must be implemented by the active daemon build. The standard v1 build includes Kademlia and configured Kademlia entries default to `enabled: true`; reduced/custom builds without it fail startup/config for such an entry.

## Desired channel subscriptions

`channels.desired` keeps selected backend broadcast subscriptions/mesh state pre-warmed across local client disconnects. It is not an IPC join, EndpointId subscription, delivery queue, or replay store.

## Effective limits and capabilities

`transport.limits.max_payload_bytes` may lower the profile's 49,152-byte ceiling. Active value is returned by capabilities.

The IPC v2 JSON-body ceiling of 131,072 bytes and IPC major version are protocol/handshake properties, not operator-selectable profile versions. Profile config may tune client counts/queues and keepalive timers within fixed ranges. By default, an EndpointId lease requires keepalive negotiation; an explicit compatibility policy may relax that requirement. A human UI using separate data-plane and admin connections consumes two client slots.

## General reload

Safe reloadable classes: supported provider config, rate/queue limits within ceilings, trust allowlist, endpoint config as above, diagnostics, desired channels.

Trust reload can close data-plane connections, affect endpoint ACL intersections, and emits `TrustPolicyChanged`. Invalid reload leaves previous good config active.

Restart-required: identity key path/rotation/restore, profile IPC socket identity, and core listen transport changes when backend cannot apply atomically. Identity recovery words are never configuration values; see `contracts/IDENTITY-RECOVERY.md`. A complete disaster-recovery plan backs up the phrase and `config.yaml` separately: the phrase restores the PeerId, while config restores trust/endpoints/discovery policy. Runtime cache/leases/messages are deliberately excluded.

## Kademlia configuration rule

The Kademlia schema is fully defined. Per ADR-0034, a configured Kademlia entry defaults to `enabled: true` in the standard v1 build; `enabled: false` is an explicit operator opt-out. Profiles may deliberately omit the provider entry entirely.

When enabled, `network_id` defines the private protocol namespace, `routing_peer_policy: data-plane-trusted` and `record_mode: disabled` remain fixed security invariants, and all documented cross-field/seed-source constraints are hard validation rules.

Endpoint IDs are never stored as Kademlia keys/provider records. Endpoint discovery uses the separate trust-gated endpoint-directory protocol.
