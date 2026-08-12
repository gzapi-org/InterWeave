# Split data-plane and administrative IPC sockets

**Status:** Accepted

## Context

Model B depends on a real distinction between ordinary data-plane clients and local transport administration. IPC v2 previously described capability grants precisely but placed both classes behind the same owner-protected socket, while `client.kind` is explicitly not cryptographic authentication. A same-socket implementation could therefore accidentally make a claimed administrative kind part of privilege selection and overstate the protection provided by capability names.

## Decision

IPC v2 uses two distinct local authority domains: `<profile>.sock` for data-plane/diagnostic traffic and `<profile>-admin.sock` for administrative traffic (named-pipe equivalents on Windows). The data socket can never grant `admin.*` regardless of `client.kind`; the admin socket cannot acquire EndpointId leases or perform ordinary direct/broadcast application messaging. Both are owner-protected by default, and deployments may apply stricter ACL/service-account policy to the admin socket.

`client.kind` remains endpoint-binding/configuration hygiene only. It is never the selector that turns a data connection into an administrator.

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

## Android amendment

The split-socket mechanism is a desktop/daemon binding. Android embedded mode has no admin socket; ADR-0041 and `contracts/LOCAL-CLIENT.md` preserve the authority split as distinct in-process `LocalDataSession` and `LocalAdminPort` interfaces, and remote event handlers are never constructed with the latter. This is a confused-deputy boundary, not a sandbox against arbitrary same-process compromise.
