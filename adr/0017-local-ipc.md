# Owner-protected UDS/named-pipe with length-prefixed JSON

**Status:** Accepted

## Context

The bridge may be TypeScript/JavaScript while the daemon is Rust. A simple generated-code-free local protocol eases debugging and keeps TCP exposure unnecessary. The IPC frame must be able to represent every transport-legal payload after base64url and JSON expansion.

## Decision

Use Unix domain sockets on Unix-like systems and named pipes on Windows, owner-restricted. Frame UTF-8 JSON with a 4-byte big-endian length. Payload bytes use base64url. Negotiate IPC major/minor and client capabilities in an initial hello.

Set the v1 JSON body ceiling to **131,072 bytes (128 KiB)**, excluding the four-byte length prefix. Keep the transport payload hard ceiling at 49,152 bytes. Define a contract invariant that a maximum-size legal payload plus maximum bounded v1 metadata must fit in both request and event frames. Ordinary `claude-channel` clients are never granted the administrative `admin.shutdown` IPC capability.

## Alternatives considered

loopback TCP; gRPC; stdio child daemon; CBOR-only custom protocol; shared memory; retain a 64 KiB JSON frame; binary payload side section in v1.

## Consequences

JSON/base64 adds overhead, but 128 KiB provides explicit headroom for a 48 KiB payload whose base64url form alone is 65,536 characters. Clear framing/versioning avoids newline ambiguity and enables push events. The ceiling is intentionally not derived from payload size at runtime; it is a separately tested protocol bound.

## Security implications

Filesystem/pipe ACLs provide cross-user isolation but not same-user process isolation. No key material crosses IPC. Loopback is disabled by default. Capability negotiation prevents ordinary Channel clients from stopping the shared daemon merely because they can issue non-administrative commands.

## Operational implications

Socket path/permissions, granted client capabilities, frame rejects, and client counts are inspectable. Bridge reconnect is straightforward; no replay cursor exists.

## Implementation implications

Maximum JSON body 128 KiB; 16 clients; bounded per-client queues; reserved control-event capacity. Phase 1 golden fixtures include exact 49,152-byte opaque payloads in outbound commands and inbound events and maximal bounded metadata to prove the fit invariant.

## Revisit conditions

Revisit if profiling shows encoding overhead material, if metadata growth threatens the frame invariant, or if cross-host daemon access becomes a real requirement (which needs a different auth model).
