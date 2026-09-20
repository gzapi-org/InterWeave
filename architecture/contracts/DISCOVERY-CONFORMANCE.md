# Discovery provider conformance contract

Every provider implementation must pass a common behavioral suite before it can be composed into DiscoveryManager.

## Mandatory guarantees

1. **Advisory only.** Discovering a peer does not make it trusted.
2. **Normalized identity.** Events contain one normalized transport PeerId, zero or more validated candidate addresses, and at most the global bounded count of advisory protocol observations.
3. **No connection-policy ownership.** The provider never establishes or tears down transport connections as a policy decision. A libp2p behaviour used by a provider mechanism may request Swarm dials, but those are backend execution and must pass ConnectionManager policy independently of the provider.
4. **No application messaging.** Provider code does not publish, subscribe, or send application payloads.
5. **Boundedness.** Provider internal state and emitted batches are bounded/configurable.
6. **Deterministic shutdown.** Cooperative cancellation stops provider tasks and closes event streams within the provider shutdown deadline.
7. **Health reporting.** Operational failures become health transitions/errors, not panics.
8. **Failure isolation.** An error from this provider does not terminate unrelated providers.
9. **Duplicate tolerance.** The provider may emit repeated observations; it must remain correct, while DiscoveryManager handles cross-provider deduplication.
10. **Malformed input safety.** Corrupt cache records, invalid addresses, malformed packets, or remote garbage do not panic the runtime.
11. **No secret leakage.** Events/logs never include private identity keys or unrelated credentials.
12. **Provenance.** Every candidate event identifies the provider source.
13. **No address-book writes.** A provider never writes the Swarm's address book: a `NetworkBehaviour`-backed provider swallows its transport behaviour's address emission at the wrapper, and a conformance test asserts a discovered pair reaches the discovery pipeline and nothing else (ADR-0011, A 2026-09-20).

## Common conformance tests

```text
provider_starts_cleanly
provider_reports_initial_health
provider_emits_normalized_candidate
provider_handles_duplicate_observation
provider_handles_candidate_update
provider_expires_when_semantics_support_ttl
provider_rejects_or_ignores_invalid_address_safely
provider_survives_malformed_provider_input
provider_respects_state_bounds
provider_shutdown_is_idempotent_and_bounded
provider_event_stream_closes_after_shutdown
provider_does_not_own_connection_policy
provider_does_not_grant_trust
provider_failure_does_not_panic
```

## Provider-specific tests

The common suite is necessary but not sufficient. mDNS adds multicast/expiry tests; cache adds corrupt-file/TTL/eviction tests; static adds config reload/address validation; Kademlia, if implemented, adds bootstrap/query/poisoning limits, capability-cache targeting, effective-target saturation, and proof that behaviour-originated dials obey root admission policy.

### The mDNS multicast tests are deferred to Stage 11

**Amended 2026-08-30.** mDNS's **expiry** tests bind from Stage 9 and are met: the provider owns normalization, dedup, bounds and expiry, and is driven by pushed observations. Its **multicast** tests are deferred to Stage 11 and are not a Stage 9 exit condition.

The reason is a dependency, not a design change. Enabling libp2p's `mdns` feature pulls `libp2p-mdns 0.48`, which pins `hickory-proto 0.25.x`, which carries RUSTSEC-2026-0118 — a DNSSEC validation loop that never terminates, with no safe upgrade available — and RUSTSEC-2026-0119, a name-compression DoS amplification fixed only in 0.26.1. No resolution exists inside the 0.48 line, so `tools/checks/check_dependencies.sh` fails, and CLAUDE.md §8 makes that a gate rather than a warning. Stage 9 therefore ships `crates/discovery/mdns` **without a socket**: the crate owns no multicast mechanism by design, and there is nothing for a packet-level test to drive.

Stage 11 owns it because that is where the libp2p feature set is next revisited under SPIKE-004, and where the dependency graph is re-resolved anyway. If the advisories are cleared sooner the tests may land sooner; the deferral names a deadline, not a preference.

**What this amendment does not do:** it does not weaken any mandatory guarantee above, and it does not make LAN discovery a proven capability. Until the multicast tests exist, mDNS is a normalization provider that has never seen a packet, and no document may describe Stage 9 as having proved LAN discovery.

**Clarified 2026-09-19.** The 2026-08-30 text above stands as written on its date; two of its sentences need scoping now that the dependency has moved, because this file was the one place in the tree still carrying them unscoped. "No safe upgrade available" is the advisory's own line about the `hickory-proto` 0.25 line that `libp2p-mdns 0.48` pins — not about hickory as such: 0.26.1 carries the RUSTSEC-2026-0119 fix, the 0.26 line is past both advisories, and `libp2p 0.57` selects `libp2p-mdns 0.49` on `hickory-proto ^0.26` (the crates.io index, read 2026-09-19; the plan records the resolution a scratch workspace measured). And "if the advisories are cleared sooner" read the clearing as an event that happens to this repository. It is not: the fix exists, taking it is a major bump of the substrate, and that was a decision Stage 11 owned and had not taken. The bump has since been taken, and the deadline this amendment set is **unmet** for a different reason: the bump clears the advisories, not the feature, and what `mdns` still lacks is the multicast mechanism Stage 9 never built (the plan's §14). The ways of closing it, with the costs, are recorded in the plan's Stage 11 obligations (`roadmap/BOTTOM-UP-IMPLEMENTATION-PLAN.md` §14), not here: this contract says what the multicast tests are and when they bind, and it still says both. The same decision clears libp2p's `dns` feature — on 0.56 it pulls `libp2p-dns 0.44` on `hickory-resolver ^0.25.2`, on 0.57 `libp2p-dns 0.45` on `^0.26` — and what the absence of `dns` does today is recorded where a reader meets it, `discovery/providers/static-bootstrap.md` §DNS ownership. Later the same day the owner ordered the bump, and it has been taken. What it clears is the advisories, not the features: `mdns` still needs the multicast mechanism Stage 9 never built, and `dns` is a transport change with no stage owner until 2026-09-20 (it has one since: p2p-network-dev, plan §14, sequenced after the mDNS mechanism) — so the multicast tests stay deferred until that mechanism exists, and the deadline stays unmet until then, one decision away rather than one bump away.

**Decision 2026-09-20.** The owner ordered the multicast mechanism built, in Stage 11, by p2p-network-dev (plan §14). `dns` was built the same day, by the same lane (plan §14); what the build does now for a `/dns4`/`/dns6` address — and the one deployment-visible change that came with it — is recorded where the 2026-09-19 text above points, `discovery/providers/static-bootstrap.md` §DNS ownership, whose two dated gap paragraphs that change deleted as they said it would. The deadline above therefore reads TAKEN-NOT-MET from this date and MET when the multicast tests in this file run against the built mechanism; the 2026-08-30 and 2026-09-19 text stands as the record of why it waited. One conformance requirement is added with the build, from ADR-0011 (A 2026-09-20): the mDNS provider MUST NOT let its transport behaviour write the Swarm's address book — `libp2p-mdns 0.49`'s `NewExternalAddrOfPeer` is swallowed at the wrapper — and a multicast test asserts that a discovered pair reaches the discovery pipeline and nothing else.
