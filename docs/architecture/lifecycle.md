# Lifecycle

## Profile lifecycle

A transport profile owns one persistent PeerId, configuration namespace, socket endpoint, mutable daemon state, and replaceable peer cache. Starting a daemon acquires a profile lock; a second daemon for the same profile fails fast and reports the existing socket/owner.

## Bridge lifecycle

1. Claude Code starts the MCP bridge over stdio.
2. Bridge resolves the configured profile/socket.
3. Bridge performs IPC version/capability handshake and receives effective transport capabilities.
4. Bridge requests subscriptions required by its configuration/session.
5. Daemon pushes normalized events while the client remains connected.
6. MCP stdin close/shutdown stops only the bridge and releases its subscription handles.
7. Daemon remains alive unless explicitly configured for ephemeral service mode or stopped by the local operator/service manager.

The bridge is never granted `admin.shutdown`; the daemon-lifetime invariant is enforced by IPC authorization, not convention alone.

## Daemon lifecycle

1. load validated normal configuration; explicitly enabled unsupported providers are fatal;
2. acquire profile lock;
3. securely load/generate identity key;
4. bind IPC endpoint with owner-only permissions;
5. start libp2p backend/listeners;
6. start DiscoveryManager providers independently;
7. begin trust-gated ConnectionManager reconciliation;
8. accept local clients and grant only client-kind-appropriate capabilities;
9. on authorized administrative shutdown: stop accepting commands, cancel providers, close direct requests, flush advisory cache best-effort, close swarm, remove socket/lock.

## Recovery

- bridge reconnect: resubscribe; no message replay;
- daemon restart: same PeerId if key is intact, cached candidates accelerate reconnect subject to trust;
- identity key missing: generate only if profile is explicitly uninitialized; otherwise fail safe if config expects an existing identity;
- corrupt key: fail closed, never silently rotate;
- provider restart: provider-local backoff and health transitions; unrelated providers remain alive;
- trust reload: emit `TrustPolicyChanged`; disconnect peers no longer authorized for ordinary data-plane connectivity.


## Optional Kademlia lifecycle

If the active build supports Kademlia and configuration remains `enabled: false`, no Kademlia provider task or protocol behavior is active. If explicitly enabled, the daemon starts the Swarm-owned driver first, injects its neutral bounded `kademlia-control-api` port into `KademliaDiscovery`, admits only trusted routing peers, and begins bootstrap/query scheduling after an eligible server seed exists. Behaviour-originated DHT dials remain subject to ConnectionManager's root dial-admission policy.

Disabling at runtime is provider-scoped: stop new queries, settle/cancel bounded in-flight work, deactivate the Kademlia behavior/protocol, expire Kademlia-only discovery provenance, and leave all other discovery providers/data-plane connections intact. Changing the Kademlia `network_id` requires a provider/behavior restart because it changes the private DHT protocol namespace.
