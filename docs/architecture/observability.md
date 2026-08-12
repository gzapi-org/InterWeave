# Observability architecture

## Principles

- structured diagnostics, stable event/counter names;
- no private keys, secret config, or payload bodies in normal logs;
- peer/channel/endpoint identifiers may be sensitive and should support redaction/hashing modes;
- Claude receives high-level status by default; local CLI/human admin may expose deeper detail.

## Status model

```text
healthy     = core configured transport capabilities usable
degraded    = partially usable; non-fatal component impaired
unavailable = core messaging cannot operate
```

Endpoint availability is reported independently so an offline `human` route does not necessarily make broadcast/unrelated endpoints unavailable.

## Diagnostic inventory

- local PeerId / identity epoch;
- configured endpoint count, active lease count, default endpoint state;
- endpoint claim/release/revoke/conflict counts;
- endpoint direct route outcomes by bounded reason class;
- endpoint directory enabled state, queries, cache hits/stale results;
- listen addresses (local admin/CLI only);
- connected/trusted peer counts;
- trust revision/policy disconnects;
- subscriptions and channel reachability;
- broadcast/direct messages in/out;
- direct protocol v2 negotiation failures;
- RemoteEndpointUnavailable and endpoint-overload classes;
- GossipSub validation outcomes;
- duplicate drops;
- IPC frame/capability/lease/keepalive diagnostics;
- direct dedup reservation occupancy/overflow and content-ID conflict counts;
- discovery/Kademlia state;
- dial/backoff failures;
- bridge/human client connected state.

## Metrics architecture

Internal facade only; no metrics backend in core.

Candidate names:

```text
connected_peers
trusted_connected_peers
configured_endpoints
active_endpoint_leases
endpoint_lease_conflicts_total
endpoint_route_no_route_total{reason_class}
endpoint_route_overloaded_total
endpoint_directory_queries_total
endpoint_directory_cache_hits_total
endpoint_directory_cache_stale_total
direct_v2_accepted_total
direct_remote_endpoint_unavailable_total
messages_received_total
messages_published_total
messages_dropped_total
dial_failures_total
provider_failures_total
validation_ignore_unauthorized_total
validation_reject_invalid_total
ipc_frame_too_large_total
ipc_keepalive_timeouts_total
direct_dedup_reservation_overflow_total
direct_duplicate_content_conflict_total
trust_policy_revision
message_no_local_consumer_total{mode}
```

Do not use raw EndpointIds as unbounded metric labels. Per-endpoint detail belongs in bounded/redacted diagnostics/status output.

## Kademlia diagnostics

Existing optional Kademlia diagnostics remain. Endpoint IDs and endpoint-directory data are not DHT diagnostics and must not be inserted into Kademlia records/labels.


Identity observability may expose algorithm, PeerId, key-file health, and whether an offline backup has been operator-acknowledged **only if that acknowledgement is non-secret state**. It must never expose recovery words, raw secret bytes, or phrase-derived material.
