# Split data-plane and administrative IPC sockets

**Status:** Accepted

## Context

Model B depends on a real distinction between ordinary data-plane clients and local transport administration. IPC v2 previously described capability grants precisely but placed both classes behind the same owner-protected socket, while `client.kind` is explicitly not cryptographic authentication. A same-socket implementation could therefore accidentally make a claimed administrative kind part of privilege selection and overstate the protection provided by capability names.

## Decision

IPC v2 uses two distinct local authority domains: `<profile>.sock` for data-plane/diagnostic traffic and `<profile>.admin.sock` for administrative traffic (named-pipe equivalents on Windows) — the `.` separator sits outside the profile-name alphabet (`[A-Za-z0-9_-]`), so no profile's data socket can share a path with another profile's admin socket, which `<profile>-admin.sock` allowed: `p-admin.sock` was both `p`'s admin socket and `p-admin`'s data socket, and the second daemon to start would have replaced the first's live socket as stale (A 2026-10-01). The data socket can never grant `admin.*` regardless of `client.kind`; the admin socket cannot acquire EndpointId leases or perform ordinary direct/broadcast application messaging. Both are owner-protected by default, and deployments may apply stricter ACL/service-account policy to the admin socket.

`client.kind` remains endpoint-binding/configuration hygiene only. It is never the selector that turns a data connection into an administrator.

The closed capability set gains `admin.status` (A 2026-09-28): a read-only administrative authority, admin socket only, so status is readable without holding a mutation capability. On Unix the connecting peer's uid MUST equal the owner uid of the runtime directory the daemon created; any other uid is closed before `hello` and counted — the same-user boundary this decision draws, with a hostile same-uid process still SPIKE-005's. The first production build implements the Unix domain socket binding only; the named-pipe equivalent, its ACL model and peer identity are carried by name to the desktop-client stage.

The closed capability set gains `admin.trust` (A 2026-10-03): the authority to read the profile's peer trust policy and to mutate it (`admin.trust.list`, `admin.trust.set`; `contracts/LOCAL-IPC.md`, IPC 2.1), admin socket only, like every `admin.*` — a data-socket `hello` asking for it is refused as any administrative claim is, under any `client.kind`. It gives the human client's settings surface (ADR-0032: trust mutation requires the platform admin binding) a method where 2.0 had none, and keeps the one invariant this record exists for: trust changes from the authority domain the operator holds, never from a data-plane session, never from an inbound message. A revoking set closes the peer's connections and is seen on the data plane only as `peer.disconnected` with `reason_class: policy`.

The split-socket mechanism is a desktop/daemon binding. Android embedded mode has no admin socket: ADR-0041 and `contracts/LOCAL-CLIENT.md` preserve the same authority split as distinct in-process `LocalDataSession` and `LocalAdminPort` interfaces, and remote event handlers are never constructed with the latter. That is a confused-deputy boundary, not a sandbox against arbitrary same-process compromise.

## Alternatives considered

One socket with capability names only; trust `client.kind=transportctl`; bearer token in normal config; require a second daemon; defer all separation to SPIKE-005.

## Consequences

Human applications that expose both messaging and settings use two connections and consume two total IPC slots. Administrative unavailability does not stop existing data-plane messaging. The protocol surface is easier to audit because admin methods are unreachable from the data socket.

## Security implications

The split prevents accidental/client-kind-based privilege crossover and gives OS-visible ACL separation. It does not cryptographically distinguish two hostile processes running as the same OS user under the default owner-only ACL; such a process may still open the admin socket. SPIKE-005 remains the explicit path for stronger same-user executable/user-presence authentication. Network payloads can never open the admin socket automatically.

## Operational implications

Operators monitor/bind two local endpoints. Admin socket ACL/bind failure degrades administrative operations but does not require taking the data socket down. Stricter deployments may place the daemon/admin client in a dedicated service account/group or use later OS-native authentication.

## Implementation implications

The IPC acceptor tags every connection with its socket authority domain before parsing `hello`. Capability grant code intersects requested capabilities with that immutable domain. Data-socket requests for `admin.*` fail `CapabilityDenied`; admin-socket endpoint claims/data messaging fail before dispatch. Total client limits count both sockets, with a separate default admin sublimit of 4.

## Revisit conditions

Revisit only to add stronger authentication within the admin domain or platform-specific privilege brokers. Do not merge sockets merely because stronger authentication is later added.

## Amendments

Full notes: [`history/0037-amendments.md`](./history/0037-amendments.md).

| Date | Amendment | Effect |
|---|---|---|
| 2026-08-12 | The authority split holds on Android without a second socket | Decision states the in-process `LocalDataSession` / `LocalAdminPort` split; confused-deputy boundary, not a sandbox |
| 2026-09-28 | admin.status is the read-only administrative authority; the peer uid is a MUST on Unix; the v1 build is Unix sockets only | Decision: `admin.status` joins the closed set as the read-only admin authority; peer uid == run-dir owner uid is a MUST on Unix (refused before hello, counted); the v1 build is UDS only, the named pipe carried to Stage 15. |
| 2026-10-01 | The admin socket is `<profile>.admin.sock`: a separator outside the profile-name alphabet | Decision: the administrative socket's name changes from `<profile>-admin.sock` to `<profile>.admin.sock`, so no two profiles' sockets can share a path; no profile name is refused; nothing shipped on the old name. |
| 2026-10-03 | admin.trust joins the closed set: trust administration over the admin socket only | Decision: `admin.trust` (read and mutate the peer trust policy; `admin.trust.list`, `admin.trust.set`, IPC 2.1) is granted on the administrative socket only; the data socket refuses it under any `client.kind`; a revoking set reaches the data plane only as `peer.disconnected` with `reason_class: policy` |
