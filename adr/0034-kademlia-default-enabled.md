# Enable Kademlia by default in the standard v1 build and profile composition

**Status:** Accepted; supersedes the rollout/default portions of ADR-0008 and ADR-0009. All Kademlia security/integration semantics in ADR-0009 remain in force.

## Context

ADR-0009 completed the Kademlia peer-routing design but kept it default-disabled while implementation risk was still unresolved. Subsequent architecture work pinned the driver/provider boundary, Swarm-wide dial admission, trusted routing population, capability observations, saturation/backoff, private namespace, record prohibition, resource limits, and SPIKE-003 evidence requirements.

The product direction now requires Kademlia to be active by default rather than an opt-in capability. A default-on profile is only coherent if the standard v1 daemon build actually contains the approved Kademlia implementation and has passed its spike/conformance/security gates.

## Decision

1. The **standard v1 daemon build MUST include** the approved `KademliaDiscovery` + Swarm-owned Kademlia driver implementation before that build can be declared release-ready.
2. In a configured `type: kademlia` provider entry, `enabled` defaults to **`true`**. Shipped remote/composite/Kademlia examples set `enabled: true` explicitly for review clarity.
3. Kademlia remains **opt-out**: an operator may set `enabled: false`, in which case no Kademlia provider task, protocol advertisement, routing-table participation, or query activity occurs for that profile.
4. Provider composition remains explicit. A profile that deliberately omits a Kademlia provider entry does not instantiate Kademlia merely because the daemon binary supports it. Minimal LAN/special-purpose profiles may therefore omit the provider entirely.
5. Reduced/custom daemon builds that omit the Kademlia implementation MUST reject a configured/defaulted `enabled: true` Kademlia entry before transport startup. They are not the standard v1 build.
6. All ADR-0009 constraints remain unchanged: private project namespace; peer-routing only; no value/provider records; no EndpointId/ChannelId/application/trust records; manual routing admission; data-plane-trusted routing peers; Swarm-wide `DialAdmissionGate`; explicit client/server role; bounded queries/saturation; advisory discovery only.
7. SPIKE-003 becomes a **v1 release gate**, not a future optional-feature spike. Kademlia conformance/security/integration tests are required before the standard v1 build ships with the default enabled.
8. Default enablement does not elevate Kademlia health to a transport-fatal runtime dependency. After successful configuration/start, a Kademlia runtime/provider failure degrades discovery while cache/mDNS/static providers and existing connections continue according to the existing failure model.

## Alternatives considered

Keep Kademlia default-disabled; make Kademlia impossible to disable; join a public DHT; use provider/value records for endpoint/channel discovery; defer implementation while shipping `enabled: true`; silently disable Kademlia on builds that do not contain it.

## Consequences

The standard v1 implementation and release scope is larger: Kademlia can no longer be postponed to a post-v1 optional phase. Remote/private deployments get distributed trusted peer-routing by default when their profile includes the provider. Operators retaining simple LAN/privacy-sensitive profiles can explicitly disable or omit it.

Default-on Kademlia also means its query/resource/privacy behavior is part of ordinary operational testing and observability, not an edge feature.

## Security implications

Default enablement increases the frequency with which Kademlia's metadata/privacy and topology-bias risks are exercised. It does **not** change the security boundary: only PeerIds already admitted by `PeerTrustPolicy` may become first-generation routing/query peers or establish behavior-originated connections, and every outbound Swarm dial remains subject to the root admission gate. Record APIs remain prohibited, and endpoint/channel/application metadata remains outside the DHT.

A compromised trusted routing peer can still bias or poison observations inside the trusted overlay. Bootstrap diversity, disjoint query paths, resource bounds, saturation logic, diagnostics, and trust revocation remain required mitigations.

## Operational implications

A standard remote profile that enables Kademlia needs a consistent `network_id` plus at least one reachable trusted server-mode seed/routing participant. Client-mode remains the default local role unless a profile intentionally configures a stable/reachable node as server.

Operators can set `enabled: false` for a profile that does not want DHT activity. That opt-out is observable and must produce zero Kademlia protocol/query activity.

## Implementation implications

Move Kademlia implementation/conformance into the v1 discovery/connection implementation sequence after SPIKE-003 rather than a final optional phase. Standard release packaging must contain `kademlia-control-api`, `discovery-kademlia`, and the Swarm driver integration. Configuration defaults/tests change to `enabled: true` for configured Kademlia entries.

Phase-1 schema tests must still cover reduced-build rejection of `enabled: true`, `enabled: false` zero-activity semantics, seed-source/cross-field validation, and default-on parsing.

## Revisit conditions

Revisit default-on posture if SPIKE-003 or production evidence shows unacceptable privacy, convergence, resource, or operational cost that cannot be corrected within the frozen trust-bounded/no-record design. Revisit the trust-bounded routing model separately if future deployments need an open discovery-only routing plane; that requires its own ADR and must not be smuggled in through this default change.
