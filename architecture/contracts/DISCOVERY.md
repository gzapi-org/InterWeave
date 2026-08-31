# Discovery contract

Status: **architecture contract, v1 draft**.

The prose here is normative for **behaviour**. The candidate and descriptor shapes are also defined as JSON Schema under [`schemas/discovery/`](./schemas/discovery/) — normative for **shape** (ADR-0049). The candidate schema is closed, which is what mechanically keeps EndpointIds, channels, roles, and presence out of discovery metadata.

Discovery is advisory reachability information. It never grants trust, sends application messages, or owns connections. It discovers **network peers/reachability only**: `EndpointId` and the endpoint-directory protocol are explicitly outside `DiscoveryProvider` and must never become discovery candidate metadata.

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
  protocol_observations?: Set<ProtocolObservation>,
}

ProtocolObservation {
  protocol_id: OpaqueTransportProtocolId,
  supported: bool,
  observed_at: Timestamp,
}
```

Candidate quality is derived from explicit provenance, freshness/expiry, address observations, and configured provider priority/cost. v1 deliberately has no generic `confidence` field because a mixed `low | normal | configured` scale duplicates provenance and can be misread as trust.

`protocol_observations` are bounded advisory transport facts learned on authenticated connections (for example, an exact Identify protocol string seen on a peer). They are **not** trust, application roles, or capability authorization. A provider that never learns such facts omits the field entirely; a provider that DOES assert them must re-assert them on every candidate it emits for that peer, because a candidate is that source's whole statement of protocol facts and an omitted fact is retracted — see `../discovery/COMPOSITION.md` for the merge rule and why the retraction is needed. The global initial cap is **16 observations per peer** and each opaque protocol identifier is capped at **256 ASCII bytes**; freshness must not outlive the candidate/cache source that supplied them.

## Events

```text
Discovered { candidate }
Updated { candidate }
Expired { peer_id, source, addresses? }
ProviderStateChanged { previous, current, reason_class? }
```

Provider event streams are the primary interface. Polling is reserved for health/diagnostics.

## Lifecycle

- `start` is called once per provider instance after validated config is available;
- a provider must emit no events before successful start;
- provider failure changes provider health but must not terminate unrelated providers;
- shutdown is cooperative and bounded; after completion the stream terminates deterministically;
- cancellation propagates from DiscoveryManager to provider tasks;
- restart/backoff belongs to DiscoveryManager policy, not to consumers of individual providers.

## add_hint

Optional ingress for observations that are meaningful to that provider. Conceptual v1 hint classes include:

```text
ObservedReachable { peer_id, address, observed_at }
ObservedProtocol { peer_id, protocol_id, supported, observed_at }
CandidateHint { candidate }
```

For example, PeerCacheDiscovery persists successful address and authenticated protocol observations; KademliaDiscovery consumes seed/capability hints routed by DiscoveryManager. Providers must reject unsupported hints explicitly rather than silently taking ownership of connection policy. Hints never grant trust.

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


## Connectivity-infrastructure boundary

Phase-9 relay/AutoNAT service authorization is **not peer discovery and not application trust**. Discovery providers may contribute ordinary address/protocol observations, but they do not add PeerIds to `transport.connectivity.infrastructure.allowed_peers`, create relay reservations, run AutoNAT probes, or initiate DCUtR. Those responsibilities stay in the libp2p connectivity/connection layer.
