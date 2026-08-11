# Discovery provider conformance contract

Every provider implementation must pass a common behavioral suite before it can be composed into DiscoveryManager.

## Mandatory guarantees

1. **Advisory only.** Discovering a peer does not make it trusted.
2. **Normalized identity.** Events contain one normalized transport PeerId and zero or more validated candidate addresses.
3. **No dialing.** The provider never establishes or tears down transport connections as a policy decision.
4. **No application messaging.** Provider code does not publish, subscribe, or send application payloads.
5. **Boundedness.** Provider internal state and emitted batches are bounded/configurable.
6. **Deterministic shutdown.** Cooperative cancellation stops provider tasks and closes event streams within the provider shutdown deadline.
7. **Health reporting.** Operational failures become health transitions/errors, not panics.
8. **Failure isolation.** An error from this provider does not terminate unrelated providers.
9. **Duplicate tolerance.** The provider may emit repeated observations; it must remain correct, while DiscoveryManager handles cross-provider deduplication.
10. **Malformed input safety.** Corrupt cache records, invalid addresses, malformed packets, or remote garbage do not panic the runtime.
11. **No secret leakage.** Events/logs never include private identity keys or unrelated credentials.
12. **Provenance.** Every candidate event identifies the provider source.

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
provider_does_not_dial
provider_does_not_grant_trust
provider_failure_does_not_panic
```

## Provider-specific tests

The common suite is necessary but not sufficient. mDNS adds multicast/expiry tests; cache adds corrupt-file/TTL/eviction tests; static adds config reload/address validation; Kademlia, if implemented, adds bootstrap/query/poisoning limits.
