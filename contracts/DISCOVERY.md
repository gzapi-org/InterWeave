# Discovery contract

Status: **architecture contract, v1 draft**.

Discovery is advisory reachability information. It never grants trust, sends application messages, or owns connections.

## Conceptual interface

Rust-oriented pseudocode, not production code:

```rust
trait DiscoveryProvider: Send {
    fn descriptor(&self) -> ProviderDescriptor;
    async fn start(&mut self, ctx: DiscoveryContext) -> Result<DiscoveryEventStream, DiscoveryError>;
    async fn add_hint(&mut self, hint: PeerHint) -> Result<HintDisposition, DiscoveryError>;
    async fn health(&self) -> DiscoveryHealth;
    async fn shutdown(&mut self) -> Result<(), DiscoveryError>;
}
```

The exact Rust signatures are intentionally left to implementation ergonomics; the behavioral contract below is normative.

## ProviderDescriptor

Required:

```text
name              stable config/provider type name
interface_version discovery contract major/minor
config_version    provider schema version only when migration is needed
```

Capability metadata is minimal and descriptive, not a control plane:

```text
scope: local | configured | network
mode: passive | active | mixed
supports_expiry: bool
supports_hints: bool
```

No independent provider implementation version is required by the runtime; build/package metadata can report it for diagnostics.

## CandidatePeer

```text
CandidatePeer {
  peer_id: TransportIdentity,
  addresses: Set<OpaqueReachabilityAddress>,
  source: ProviderName,
  observed_at: Timestamp,
  expires_at?: Timestamp,
  confidence?: low | normal | configured,
}
```

`confidence` describes provenance quality only. It is not trust. `configured` means an operator supplied the hint; it does not authorize messages.

## Events

```text
Discovered { candidate }
Updated { candidate }
Expired { peer_id, source, addresses? }
ProviderStateChanged { previous, current, reason_class? }
```

Provider event streams are the primary interface. Polling is reserved for health/diagnostics.

## Lifecycle

- `start` is called once per provider instance after validated config is available.
- a provider must emit no events before successful start;
- provider failure changes provider health but must not terminate unrelated providers;
- shutdown is cooperative and bounded; after completion the stream terminates deterministically;
- cancellation propagates from DiscoveryManager to provider tasks;
- restart/backoff belongs to DiscoveryManager policy, not to consumers of individual providers.

## add_hint

Optional ingress for observations that are meaningful to that provider (for example, the peer-cache provider can persist a successful address observation). Providers must reject unsupported hints explicitly rather than silently taking ownership of connection policy.

## Health

Provider health:

- `healthy`: provider can perform its configured function;
- `degraded`: partially functional or recently failing but may still emit useful candidates;
- `unavailable`: cannot currently provide candidates.

Aggregate discovery health is computed by DiscoveryManager. A transport can be healthy with discovery degraded if sufficient peers are already connected.

## Prohibited behavior

A provider must not:

- grant transport trust or channel membership;
- dial/disconnect peers directly;
- own the libp2p Swarm;
- subscribe/publish GossipSub;
- send direct application messages;
- expose application roles/business metadata;
- mutate Claude sessions;
- become a bootstrap authority or membership server.
