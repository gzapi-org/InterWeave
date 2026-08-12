# Owner-protected UDS/named-pipe with endpoint-aware length-prefixed JSON

**Status:** Accepted; IPC major version advanced by ADR-0030.

## Context

The bridge/human client may be TypeScript/JavaScript while the daemon is Rust. A generated-code-free local protocol eases debugging. Model B additionally requires an exclusive configured EndpointId lease per direct-capable client and must still represent the maximum payload with endpoint metadata.

## Decision

Use Unix domain sockets / Windows named pipes with owner restrictions. Frame UTF-8 JSON with a four-byte big-endian length; payload bytes use base64url.

Keep the JSON body ceiling at **131,072 bytes (128 KiB)**. Advance implementation target to **IPC v2**. The hello handshake optionally claims one configured EndpointId; direct-capable clients require a successful exclusive lease. Handshake errors are precise locally (`EndpointUnknown`, `EndpointDisabled`, `EndpointClientKindDenied`, `EndpointInUse`, `CapabilityDenied`) while remote direct routing keeps the coarse `no_route` privacy class. Capabilities remain authorization-relevant: human-client receives `endpoints.query` by default only when endpoint directory is enabled; claude-channel does not; `admin.endpoints`/`admin.shutdown` require explicit administrative policy and are never granted to Claude Channel data-plane clients. IPC version is negotiated in hello, not configured as an operator profile value.

IPC v2 may negotiate bounded server ping/client pong keepalive. Default timers are 30s interval, 10s response timeout, three misses. Expiry closes the connection and releases its endpoint lease; keepalive is liveness only, not authentication.

## Alternatives considered

Loopback TCP; gRPC; stdio child daemon; CBOR-only protocol; shared memory; 64 KiB JSON; binary side section; separate socket per EndpointId; dynamic arbitrary endpoint registration.

## Consequences

One profile socket multiplexes multiple local applications while exact endpoint ownership is explicit. JSON overhead remains acceptable under the tested 128 KiB frame invariant.

## Security implications

ACLs provide cross-user isolation but not full same-user isolation. Configured-only exclusive endpoint leases prevent accidental route collisions. Client kind is hygiene, not authentication. Admin endpoint/shutdown authority is capability-separated from data plane.

## Operational implications

Socket permissions, client kinds, endpoint leases/epochs, capability grants, conflicts, frame rejects, and client counts are inspectable. Reconnect receives a fresh lease epoch; no replay exists.

## Implementation implications

16 default IPC **connections**, one endpoint lease/data-plane connection, bounded per-client queues, reserved control events. A human app using separate data and admin sessions consumes two slots. Golden fixtures include exact handshake error codes, default capability grants, keepalive behavior, max payload plus 64-byte source/destination EndpointIds. Direct queue overload rejects before network acceptance.

## Revisit conditions

Revisit if stronger same-user client identity, shared endpoint leases, binary payload framing, or cross-host daemon access becomes required.
