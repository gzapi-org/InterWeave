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
- subscription count and per-channel high-level reachability;
- messages broadcast/direct in/out;
- publish/direct failure classes;
- duplicate drops;
- overload/drop counters by boundary;
- discovery provider states and last success;
- candidates/addresses learned/expired by provider;
- dial attempts/failures/backoff classes;
- bootstrap candidate reachability;
- GossipSub mesh peer counts by hashed/redacted channel key;
- direct protocol negotiation failures;
- IPC connected client count/version;
- bridge/daemon connection state.

## Metrics abstraction

Metrics are part of the architecture, but no Prometheus dependency is in core contracts. A neutral recorder can emit counters/gauges/histograms to logs, OpenTelemetry, Prometheus adapter, or tests.

Suggested names:

```text
connected_peers
discovered_peers_total{provider}
messages_received_total{mode}
messages_sent_total{mode}
messages_dropped_total{reason,boundary}
dial_failures_total{class}
provider_failures_total{provider}
ipc_clients
queue_depth{boundary}
direct_latency_ms
```
