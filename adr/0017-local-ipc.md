# Owner-protected UDS/named-pipe with endpoint-aware length-prefixed JSON

**Status:** Accepted; IPC major version advanced by ADR-0030 and authority topology amended by ADR-0037.

## Context

The bridge/human client may be TypeScript/JavaScript while the daemon is Rust. A generated-code-free local protocol eases debugging. Model B additionally requires an exclusive configured EndpointId lease per direct-capable client and must still represent the maximum payload with endpoint metadata.

## Decision

Use two owner-protected Unix domain sockets / Windows named pipes per profile: a data-plane socket and a separate administrative socket. Frame UTF-8 JSON with a four-byte big-endian length; payload bytes use base64url. The data socket can never grant `admin.*`; the admin socket cannot own EndpointId leases or application messaging.

Keep the JSON body ceiling at **131,072 bytes (128 KiB)**. Advance implementation target to **IPC v2**. The data-socket hello handshake optionally claims one configured EndpointId; direct-capable clients require a successful exclusive lease. Handshake errors are precise locally (`EndpointUnknown`, `EndpointDisabled`, `EndpointClientKindDenied`, `EndpointInUse`, `CapabilityDenied`) while remote direct routing keeps the coarse `no_route` privacy class. Capabilities remain authorization-relevant: human-client receives `endpoints.query` by default only when endpoint directory is enabled; claude-channel does not. Per ADR-0037, `admin.endpoints`/`admin.shutdown` exist only on the separate admin socket and can never be obtained by claiming an administrative `client.kind` on the data socket. IPC version is negotiated in hello, not configured as an operator profile value.

IPC v2 negotiates bounded server ping/client pong keepalive. Default timers are 30s interval, 10s response timeout, three misses. **Profile policy defaults to requiring keepalive negotiation for any connection claiming an EndpointId lease**; omission then yields local `CapabilityDenied` before lease grant. Endpoint-less admin/diagnostic sessions are unaffected, and operators may explicitly relax the requirement for third-party compatibility. Expiry closes the connection and releases its endpoint lease; keepalive is liveness only, not authentication.

## Alternatives considered

Loopback TCP; gRPC; stdio child daemon; CBOR-only protocol; shared memory; 64 KiB JSON; binary side section; separate socket per EndpointId; dynamic arbitrary endpoint registration.

## Consequences

One profile **data socket** multiplexes multiple local applications while exact endpoint ownership is explicit; a second profile admin socket isolates administrative methods. JSON overhead remains acceptable under the tested 128 KiB frame invariant.

## Security implications

ACLs provide cross-user isolation but not full same-user isolation. Configured-only exclusive endpoint leases prevent accidental route collisions. Requiring keepalive for leased data-plane endpoints by default bounds first-party half-open lease retention, but it is not client authentication. Client kind is hygiene, not authentication. Admin endpoint/shutdown authority is socket-domain-separated from the data plane; same-UID code able to open the admin socket remains a documented residual risk.

## Operational implications

Data/admin socket permissions, client kinds, endpoint leases/epochs, capability grants, cross-domain denials, conflicts, frame rejects, and client counts are inspectable. Reconnect receives a fresh lease epoch; no replay exists.

## Implementation implications

16 default IPC **connections total** across data/admin sockets, at most 4 admin connections by default, one endpoint lease/data-plane connection, bounded per-client queues, reserved control events. A human app using separate data and admin sockets consumes two slots. Golden fixtures include cross-domain capability denial, exact handshake error codes, default capability grants, keepalive behavior, max payload plus 64-byte source/destination EndpointIds. Direct queue overload rejects before network acceptance.

## Revisit conditions

Revisit if stronger same-user client identity, shared endpoint leases, binary payload framing, or cross-host daemon access becomes required.
