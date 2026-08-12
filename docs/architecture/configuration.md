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
| runtime IPC endpoints | data socket/pipe + admin socket/pipe | no | no | recreated |
| human app retention store | pending outbound, unread inbound, receiver-kept inbound | separate app policy; not transport profile backup | **yes, message content** | no for surviving states |
| logs | structured diagnostics | policy-dependent | must be sanitized | yes |


## Identity algorithm and recovery

The initial software profile algorithm is fixed to `ed25519`. The key file remains identity data, not YAML secret material. Standard v1 fixes `identity.key_protection=filesystem-only`; ADR-0038 makes a passphrase-encrypted key envelope an explicit v2.x direction, but it is not selectable until SPIKE-007 pins an audited external format/library and unlock path.

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

The IPC v2 JSON-body ceiling of 131,072 bytes and IPC major version are protocol/handshake properties, not operator-selectable profile versions. `ipc.socket_layout` is fixed to `split-data-admin`: data and admin sockets are separate authority domains, with `admin.*` impossible on the data socket regardless of `client.kind`. Profile config may tune total/admin client counts, queues, and keepalive timers within fixed ranges. By default, an EndpointId lease requires keepalive negotiation; an explicit compatibility policy may relax that requirement. A human UI using separate data-plane and admin sockets consumes two client slots.

## Network abuse-control configuration

`transport.pre_auth` bounds listener work before Noise can authenticate a PeerId: 64 pending globally, 8/source bucket, 10-second timeout, 30 starts/minute/source, and 600 starts/minute globally by default. IPv6 source buckets use /64. These limits are not trust decisions and cannot update peer backoff because no authenticated peer exists yet.

`transport.connection_policy` keeps address-level backoff separate from peer punitive state and uses a 30-minute identity-mismatch quarantine by default. `transport.direct.inbound_rate_limit` is mandatory for trusted direct peers (120/minute burst 32 per PeerId; 1200/minute burst 256 global defaults). Reloading these values must preserve hard ceilings and existing in-flight safety.

## General reload

Safe reloadable classes: supported provider config, rate/queue limits within ceilings, trust allowlist, endpoint config as above, diagnostics, desired channels.

Trust reload can close data-plane connections, affect endpoint ACL intersections, and emits `TrustPolicyChanged`. Invalid reload leaves previous good config active.

Restart-required: identity key path/rotation/restore, profile IPC socket identity, and core listen transport changes when backend cannot apply atomically. Identity recovery words are never configuration values; see `contracts/IDENTITY-RECOVERY.md`. A complete disaster-recovery plan backs up the phrase and `config.yaml` separately: the phrase restores the PeerId, while config restores trust/endpoints/discovery policy. Runtime cache/leases/transport messages are deliberately excluded. Human application retention is separately governed by ADR-0044 and is not part of transport profile recovery.

## Mandatory Internet reachability configuration

Standard-v1 profiles use the Phase-9 connectivity stack from ADR-0035. The schema therefore treats the client roles as required capabilities rather than optional feature toggles:

- `transport.connectivity.required` is fixed `true`;
- AutoNAT v2 client is enabled;
- Circuit Relay v2 client/reservation management is enabled;
- DCUtR is enabled;
- server roles for AutoNAT and relay remain explicit opt-in infrastructure roles;
- `transport.connectivity.infrastructure.allowed_peers` authorizes control-plane infrastructure without granting application data-plane trust.

Static AutoNAT server and relay PeerIds must be members of either `trust.allowed_peers` or the infrastructure allowlist. A peer present in both sets is treated as data-plane trusted. Discovery or Identify observations never edit either authorization set.

Cross-field validation includes relay target ordering/caps, probe/reservation retry ordering, server per-peer limits not exceeding global limits, DCUtR per-peer concurrency not exceeding global concurrency, and authorization of every statically configured service PeerId. Invalid combinations fail configuration before network startup.

Connectivity configuration is reloadable only where the runtime can make the transition atomically. Changing infrastructure authorization may close control connections and cancel associated probes/reservations. Lowering relay targets retires surplus reservations only after a replacement/direct path is stable; raising targets schedules bounded acquisition. Enabling an AutoNAT/relay server role may require listener/service restart if the backend cannot safely add the role live.

`ConnectivitySummary` is runtime state, not persisted configuration. AutoNAT observations, relay reservations, DCUtR attempts, and relay-derived listen addresses expire/rebuild after restart or network change.

## Kademlia configuration rule

The Kademlia schema is fully defined. Per ADR-0034, a configured Kademlia entry defaults to `enabled: true` in the standard v1 build; `enabled: false` is an explicit operator opt-out. Profiles may deliberately omit the provider entry entirely.

When enabled, `network_id` defines the private protocol namespace, `routing_peer_policy: data-plane-trusted` and `record_mode: disabled` remain fixed security invariants, and all documented cross-field/seed-source constraints are hard validation rules.

Endpoint IDs are never stored as Kademlia keys/provider records. Endpoint discovery uses the separate trust-gated endpoint-directory protocol.

## Human message retention is not operator configuration

ADR-0044 retention semantics are first-party application invariants, not profile toggles: pending outbound is durable until transport-terminal, inbound unread is durable until read, and read inbound is durable only after the receiver explicitly chooses Keep. Standard v1 does not provide a configuration flag that silently turns permanent chat history back on. Any broader retention/history mode requires a new ADR/application policy.

Android system backup remains disabled for the entire human-store. Any future explicit encrypted message backup may include only inbound unread and receiver-kept content; pending outbound is local restart state and not portable backup state.

## Human platform deployment

`runtime.deployment` selects only the local process binding: `daemon-ipc` (desktop/server default) or `embedded-android`. It does not change network protocols. Android embedded mode disables IPC, requires an enabled configured human EndpointId, uses Kademlia client mode, and disables relay/AutoNAT server roles. `stay-reachable` selects the user-visible Android remote-messaging foreground-service lifecycle; `foreground-only` intentionally permits the route to disappear when the app runtime stops. `stay-reachable + key_unlock_policy=user-presence` is valid but yields the stable derived diagnostic `background_restart_requires_user_authentication=true`; system backup/device-transfer exclusions are packaging invariants, not operator-tunable profile settings.

Infrastructure Identify auto-candidate flags default **false**. Static configured AutoNAT servers/relays are preferred. Explicit opt-in to Identify-learned authorized infrastructure is a topology convenience, never trust promotion.
