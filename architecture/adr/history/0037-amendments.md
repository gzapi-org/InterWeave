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
