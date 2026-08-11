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
```

Labels must be bounded; never put raw payloads, private keys, arbitrary peer-supplied strings, or unbounded ChannelIds into metric label cardinality.
