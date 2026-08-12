# Lifecycle

## Profile lifecycle

A transport profile owns one persistent PeerId, configuration namespace, socket endpoint, endpoint configuration, mutable daemon state, and replaceable peer cache. Starting a daemon acquires a profile lock; a second daemon for the same profile fails fast.

## Local endpoint lifecycle

Configured EndpointIds exist independently of client processes. A route becomes **available** only while one IPC v2 connection owns its exclusive lease.

```text
configured+enabled
   |
   | IPC hello claim
   v
leased / routable / optionally advertised
   |
   | disconnect, config disable, admin revoke, daemon stop
   v
configured but unavailable
```

No messages are retained while unavailable.

## Claude bridge lifecycle

1. Claude Code starts MCP bridge over stdio.
2. Bridge resolves profile/socket and its configured EndpointId.
3. Bridge performs IPC v2 handshake, requests EndpointId lease, capabilities, and receives profile identity/effective limits.
4. Bridge requests its channel subscriptions.
5. Daemon pushes broadcasts matching joins and direct events addressed to that EndpointId.
6. MCP stdin close stops bridge, releases endpoint lease and joins.
7. Daemon remains alive.

A second bridge cannot claim the same EndpointId. It must have another configured route if simultaneous direct addressability is desired.

## Human client lifecycle

Human data-plane client follows the same lease model. UI restart releases then reacquires `human` without changing PeerId. Its optional local application message/history database is independent from daemon state.

Administrative settings use a separately capability-authorized IPC connection; network messages do not automatically exercise that authority.

## Daemon lifecycle

1. load config schema v2; validate endpoints/trust/providers;
2. acquire profile lock;
3. securely load/generate identity key;
4. bind owner-protected IPC endpoint;
5. start libp2p backend/listeners including direct v2 and optional endpoint-directory behavior;
6. start discovery providers independently;
7. begin trust-gated ConnectionManager reconciliation;
8. accept IPC clients, grant capabilities, and establish exclusive endpoint leases;
9. on authorized shutdown: stop new claims/commands, stop directory exposure, revoke endpoint leases, cancel providers/new dials, settle bounded direct responses, close Swarm, remove socket/lock.

## Recovery

- local client reconnect: fresh handshake, fresh non-repeating 128-bit endpoint lease epoch, and joins; no replay;
- daemon restart: same PeerId if key intact; all endpoint leases start offline until clients reconnect;
- remote endpoint directory cache: discarded on daemon restart and naturally expires;
- corrupt/missing identity: existing fail-closed rules apply;
- trust reload: disconnect unauthorized peers and recompute endpoint policy intersections;
- endpoint config reload: revoke now-invalid lease; never auto-rebind.

## Optional Kademlia lifecycle

Unchanged by Model B. Kademlia remains disabled unless explicitly enabled on a supporting build. Endpoint records are never placed in the DHT.
