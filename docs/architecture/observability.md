# Observability architecture

## Principles

- structured diagnostics, stable event/counter names;
- no private keys, secret config, or payload bodies in normal logs;
- peer/channel identifiers may be sensitive and should support redaction/hashing modes;
- Claude receives only high-level status by default; local CLI can expose deeper network detail.

## Status model

Top-level:

```text
healthy   = core transport usable for configured capabilities
degraded  = partially usable; one/more non-fatal components impaired
unavailable = core messaging cannot operate
```

Provider health uses the same terms independently.

## Diagnostic inventory

- local PeerId and identity epoch;
- listen addresses (local CLI only by default);
- connected/trusted peer counts;
- unauthorized connection refusals / policy disconnects;
- trust-policy revision and last change timestamp;
- subscription count and per-channel high-level reachability;
- messages broadcast/direct in/out;
- publish/direct failure classes, including `UnauthorizedPeer` and `ChannelNotJoined`;
- GossipSub validation outcomes: `validation_accept`, `validation_ignore_unauthorized`, `validation_reject_invalid`;
- duplicate drops;
- overload/drop counters by boundary;
- IPC frame-too-large rejects and granted client-capability summaries;
- discovery provider state/health;
- candidates learned/expired per provider;
- configured bootstrap candidates;
- dial attempts/failures/backoff classes, including DNS/address-resolution failures;
- GossipSub mesh peer count by locally joined topic (bounded/redacted output);
- direct protocol negotiation failures;
- IPC connected client count/kind;
- direct-message local fan-out count and no-local-consumer drops;
- broadcast no-local-consumer drops for profile-desired channels;
- Channel bridge connected/degraded state.

## Metrics architecture

The runtime emits internal counters/gauges/events through an observability facade. No Prometheus dependency belongs in transport/discovery core. A later daemon adapter may expose Prometheus/OpenTelemetry/log-only output.

Candidate names include:

```text
connected_peers
trusted_connected_peers
discovered_peers_total
messages_received_total
messages_published_total
messages_dropped_total
dial_failures_total
provider_failures_total
validation_ignore_unauthorized_total
validation_reject_invalid_total
ipc_frame_too_large_total
trust_policy_revision
direct_local_fanout_deliveries_total
message_no_local_consumer_total{mode}
```

Labels must be bounded; never put raw payloads, private keys, arbitrary peer-supplied strings, or unbounded ChannelIds into metric label cardinality.


## Kademlia diagnostics (when compiled and enabled)

```text
kademlia_enabled
kademlia_mode
kademlia_protocol_hash
kademlia_routing_peers
kademlia_nonempty_buckets
kademlia_bootstrap_total
kademlia_bootstrap_failures_total
kademlia_last_bootstrap_success
kademlia_queries_started_total{class}
kademlia_queries_completed_total{class}
kademlia_query_failures_total{class,reason}
kademlia_query_timeouts_total{class}
kademlia_candidates_emitted_total
kademlia_candidates_expired_total
kademlia_routing_insert_denied_total{reason}
kademlia_record_write_attempts_total{kind}
kademlia_driver_channel_overflow_total
kademlia_effective_routing_target
kademlia_saturation_state
kademlia_behaviour_dial_requests_total
kademlia_behaviour_dial_denied_total{reason}
kademlia_behaviour_dial_connected_total
kademlia_targeted_lookup_skipped_total{reason}
```

Do not expose random lookup keys as ordinary metric labels/log fields. `network_id` and protocol hash are local diagnostics, not secrets or trust proof.
