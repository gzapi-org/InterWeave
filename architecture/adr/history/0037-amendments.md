# ADR-0037 — amendment history

### Amendment 2026-08-12 — The authority split holds on Android without a second socket

The split-socket mechanism is a desktop/daemon binding, and Android embedded mode has no admin socket. ADR-0041 and `contracts/LOCAL-CLIENT.md` preserve the same authority split as distinct in-process `LocalDataSession` and `LocalAdminPort` interfaces, with remote event handlers never constructed with the latter. The Decision section is amended to say so, so that a reader does not conclude the separation is desktop-only.

The scope note is deliberate: in-process separation is a **confused-deputy boundary, not a sandbox** against arbitrary same-process compromise. The decision's substance — that a data connection can never obtain `admin.*` authority, and that `client.kind` is never the selector that grants it — is unchanged on both platforms.

The text arrived as a trailing `## Android amendment` section, a convention predating ADR-0048. It is now folded into the Decision and recorded here.

### Amendment 2026-09-28 — admin.status is the read-only administrative authority; the peer uid is a MUST on Unix; the v1 build is Unix sockets only

**Trigger.** Stage 13 (plan §16 (5)–(6), (9)) needed a way for
`transportctl status` to read the daemon without holding a mutation
capability, a decision on whether peer credentials are advisory or
binding, and a stated platform scope for the first build.

**What changed.** `admin.status` joins the closed capability set
(`ipc/capability` 1.1.0) as the read-only administrative authority,
carrying the raw detail `LOCAL-IPC.md` reserves for a diagnostics/admin
capability; the owner decided it on 2026-09-28. Peer credentials become
a MUST on Unix: the peer uid must equal the run directory's owner uid,
or the connection is closed before `hello` and counted
(`ipc_peer_credential_refused_total`) — `LOCAL-IPC.md` had said "should
be inspected where the OS exposes them". The first production build is
Unix domain sockets only; the Windows named pipe, its ACL model and peer
identity are carried by name to Stage 15 (§18).

**Not changed.** The two-domain topology, the data socket's categorical
ineligibility for `admin.*`, the Android in-process split.

### Amendment 2026-10-01 — The admin socket is `<profile>.admin.sock`: a separator outside the profile-name alphabet

p2p-network-dev found, building the daemon (#156), that the two socket
names met across profiles: with a profile name admitting `-`,
`work-admin.sock` was both `work`'s admin socket and `work-admin`'s
data socket, and the lock holder's stale-socket replacement — which
judged only "a socket of this uid" — would have unlinked `work`'s live
admin socket and bound `work-admin`'s data socket in its place. #156
first made the replacement refuse a socket that accepts a connection
(the second daemon exits naming a live socket), which removed the
destruction but left the two profiles unable to run at once for one
user, whichever started first winning (GZCoord 01a0f462).

The Decision read "`<profile>-admin.sock` for administrative traffic".
Ruled (01a0f463): the administrative socket is `<profile>.admin.sock`.
The profile-name alphabet is `[A-Za-z0-9_-]`, so `.` cannot occur in a
name and `<p>.sock` and `<q>.admin.sock` are never one path for any p
and q — the collision is removed by construction. Rejected: refusing
names that end in `-admin` (a socket layout leaking into the profile
namespace, and a break for whoever holds such a name) and per-profile
subdirectories (a second directory with its own mode rules for two
files). Nothing shipped on the old name, so there is no migration; the
liveness refusal stays as the guard on the stale-socket path whatever
the names are. The Decision's sentence carries the new name and why;
LOCAL-IPC.md's socket line, LOCAL-CLIENT.md's desktop binding block and
the human-client-desktop diagram follow it.

### Amendment 2026-10-03 — admin.trust joins the closed set: trust administration over the admin socket only

IPC 2.0 shipped with no trust method: `LOCAL-IPC.md` said trust and discovery administration (ADR-0032) "have no method in v2.0; they are Stage 15's". Stage 15's desktop client needs the human settings surface to read and change peer trust, and ADR-0032 already requires that to happen through the platform admin binding. The question this record answers is only which authority domain the new methods belong to, and the answer is the one it has always given: the administrative socket, under a capability of its own.

The closed capability set gains `admin.trust`, granting `admin.trust.list` (the allowlist as `PeerTrustPolicy` holds it; deny-by-default is the policy's shape and is not reported as a setting) and `admin.trust.set {peer, allowed}` (IPC 2.1; `TrustDecision` and `DenyReason` stay local diagnostics and never cross the wire). It is refused on the data socket under any `client.kind`, like every `admin.*`. A set that revokes closes the peer's connections, and the data plane learns of it only through `peer.disconnected` with `reason_class: policy` — a data-plane session cannot tell an administrative revocation from one made by configuration, which is the point. The methods are the same runtime overlay as `admin.endpoints.*` until the owner decides persistence (ADR-0028). The schemas and the enums' minor bumps land `approved` with the implementing batch and its Rust mirror (plan §18, Stage 15's R2), as every 2.0 shape did.
