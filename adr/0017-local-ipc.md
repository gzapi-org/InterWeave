# Owner-protected UDS/named-pipe with endpoint-aware length-prefixed JSON

**Status:** Accepted; IPC major version advanced by ADR-0030.

## Context

The bridge/human client may be TypeScript/JavaScript while the daemon is Rust. A generated-code-free local protocol eases debugging. Model B additionally requires an exclusive configured EndpointId lease per direct-capable client and must still represent the maximum payload with endpoint metadata.

## Decision

Use Unix domain sockets / Windows named pipes with owner restrictions. Frame UTF-8 JSON with a four-byte big-endian length; payload bytes use base64url.

Keep the JSON body ceiling at **131,072 bytes (128 KiB)**. Advance implementation target to **IPC v2**. The hello handshake optionally claims one configured EndpointId; direct-capable clients require a successful exclusive lease. Capabilities remain authorization-relevant, including `admin.endpoints` and `admin.shutdown`, neither of which is granted to Claude Channel data-plane clients.

## Alternatives considered

Loopback TCP; gRPC; stdio child daemon; CBOR-only protocol; shared memory; 64 KiB JSON; binary side section; separate socket per EndpointId; dynamic arbitrary endpoint registration.

## Consequences

One profile socket multiplexes multiple local applications while exact endpoint ownership is explicit. JSON overhead remains acceptable under the tested 128 KiB frame invariant.

## Security implications

ACLs provide cross-user isolation but not full same-user isolation. Configured-only exclusive endpoint leases prevent accidental route collisions. Client kind is hygiene, not authentication. Admin endpoint/shutdown authority is capability-separated from data plane.

## Operational implications

Socket permissions, client kinds, endpoint leases/epochs, capability grants, conflicts, frame rejects, and client counts are inspectable. Reconnect receives a fresh lease epoch; no replay exists.

## Implementation implications

16 default clients, one endpoint lease/client, bounded per-client queues, reserved control events. Golden fixtures include max payload plus 64-byte source/destination EndpointIds. Direct queue overload rejects before network acceptance.

## Revisit conditions

Revisit if stronger same-user client identity, shared endpoint leases, binary payload framing, or cross-host daemon access becomes required.
