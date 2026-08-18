# local-client-api

Neutral `LocalDataSession` / `LocalAdminPort` boundary shared by desktop IPC and the Android in-process adapter.

**Current status:** Stage 1, active workspace member. Types and authorization decisions only — no sockets, no processes, no I/O.

## Why it is platform-free

`contracts/LOCAL-CLIENT.md` exists because the semantic boundary must be identical whether it is serialized over a socket or called in-process. A socket, process, or JNI type in this crate would make the two bindings different contracts wearing one name.

## Two authority rules, made structural

**A data session cannot reach administrative authority.** `LocalDataSession` has no method, field, or capability yielding a `LocalAdminPort` — and `admin.*` is not *representable* in a data session's grant, because `DataCapability` does not contain those variants. `AdminCapability` is a separate enum, so the two cannot even share a collection. This is not a runtime check someone could forget; a handshake response naming `admin.shutdown` fails to deserialize into a data session, and there is a test asserting exactly that.

**Source endpoint comes from the lease, never from a caller.** No constructor, setter, or parameter in this crate accepts a caller-supplied source endpoint. ADR-0030 puts the non-spoofable source here, and an override "for testing" would be the hole it guards. A session with no lease cannot send at all — `EndpointNotRegistered`, before anything reaches the network — rather than sending as nobody.

## Details worth knowing

- **`client_kind` is a hygiene label, not authentication.** A session may call itself `admin`; it is still a data session, because authority lives in the capability set.
- **A zero-length event queue is refused.** It reads like "unbounded" and behaves like "closed": every direct message to that session would be rejected.
- **`LeaseRefusal` is local detail only.** `EndpointUnknown` / `Disabled` / `InUse` are precise here and all become `no_route` on the wire — a test walks every variant through `to_wire()` to prove it, since distinguishing them remotely would reveal which endpoints exist and which are occupied.
- **`LocalAdminPort::endpoint_lease()` is a `const fn` returning `None`**, not a field. Administrative connections never hold application leases, so there is no state that could drift from the rule.
