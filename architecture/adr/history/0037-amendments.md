# ADR-0037 — amendment history

### Amendment 2026-08-12 — The authority split holds on Android without a second socket

The split-socket mechanism is a desktop/daemon binding, and Android embedded mode has no admin socket. ADR-0041 and `contracts/LOCAL-CLIENT.md` preserve the same authority split as distinct in-process `LocalDataSession` and `LocalAdminPort` interfaces, with remote event handlers never constructed with the latter. The Decision section is amended to say so, so that a reader does not conclude the separation is desktop-only.

The scope note is deliberate: in-process separation is a **confused-deputy boundary, not a sandbox** against arbitrary same-process compromise. The decision's substance — that a data connection can never obtain `admin.*` authority, and that `client.kind` is never the selector that grants it — is unchanged on both platforms.

The text arrived as a trailing `## Android amendment` section, a convention predating ADR-0048. It is now folded into the Decision and recorded here.
