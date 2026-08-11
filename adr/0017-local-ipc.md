# Owner-protected UDS/named-pipe with length-prefixed JSON

**Status:** Accepted

## Context

The bridge may be TypeScript/JavaScript while the daemon is Rust. A simple generated-code-free local protocol eases debugging and keeps TCP exposure unnecessary.

## Decision

Use Unix domain sockets on Unix-like systems and named pipes on Windows, owner-restricted. Frame UTF-8 JSON with a 4-byte big-endian length. Payload bytes use base64url. Negotiate IPC major/minor in an initial hello.

## Alternatives considered

loopback TCP; gRPC; stdio child daemon; CBOR-only custom protocol; shared memory.

## Consequences

JSON/base64 adds overhead but messages are capped at 48 KiB. Clear framing and versioning avoid newline ambiguity and enable push events.

## Security implications

Filesystem/pipe ACLs provide cross-user isolation but not same-user process isolation. No key material crosses IPC. Loopback is disabled by default.

## Operational implications

Socket path/permissions and client counts are inspectable. Bridge reconnect is straightforward; no replay cursor exists.

## Implementation implications

Maximum frame 64 KiB; 16 clients; bounded per-client queues; reserved control-event capacity.

## Revisit conditions

Revisit if profiling shows encoding overhead material, or if cross-host daemon access becomes a real requirement (which needs a different auth model).
