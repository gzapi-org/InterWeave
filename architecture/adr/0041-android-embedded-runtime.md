# Android human client embeds TransportRuntime in a foreground service

**Status:** Accepted; platform-specific amendment to ADR-0015/0032/0037

## Context

The desktop standalone-daemon model maps poorly to Android background/process rules. Android nevertheless needs the same PeerId/EndpointId/network semantics and, when the user asks to remain reachable, a user-visible lifecycle owner independent of the Activity window.

## Decision

The Android first-party app embeds the Rust `TransportRuntime` inside an Android foreground service host rather than launching a standalone daemon or exposing local TCP/UDS IPC. The UI talks to the runtime through the neutral `LOCAL-CLIENT` in-process adapter. The service owns the `human` EndpointId lease while active.

Continuous background reachability is explicit user opt-in and uses the current Android `remoteMessaging` foreground-service category subject to SPIKE-008 target-SDK/Play-policy validation. Foreground-only mode is supported. On API 37+ `INTERNET` is a runtime permission (A 2026-10-10): the Activity asks for network access before it binds the Service, a denial opens the store without a network in a Service that runs no runtime and holds no lease (reading, Keep and Unkeep work as ADR-0044 defines; nothing sends or joins), and a stay-reachable Service restarted without the permission posts one notification and ends (`human-client-android.md` §Runtime permissions). Where the opt-in LIVES (A 2026-10-10): `runtime.android.availability_mode` in the profile's `config.yaml` is the authored default, never rewritten by the host or the runtime; the person's choice is a persisted overlay in the profile's state directory under the trust boundary (the trust overlay's sibling, ADR-0028), written through one embedded-host call on the lifecycle seam, read as one effective mode (overlay, else config) by everything derived from it; turning it off removes the entry; a change while running restarts the service in the new mode. No centralized push wake-up dependency is added. `stay-reachable` may be combined with `user-presence` key unlock, but that combination must expose `background_restart_requires_user_authentication=true` and must not claim automatic reachability after process/service restart.

## Alternatives considered

Standalone daemon process; loopback TCP daemon; Kotlin/JVM networking implementation; WorkManager as permanent socket owner; FCM-backed wakeup; make Android foreground-only.

## Consequences

Network protocols remain identical to desktop, but the local process boundary differs. Activity recreation does not imply transport restart while the service lives. Service/process loss makes the endpoint offline; no mailbox appears.

## Security implications

Android in-process data/admin separation prevents confused-deputy wiring but cannot sandbox arbitrary code execution within the same APK process. Remote event handlers are constructed without admin capability. OS service state and notification interactions are local platform events, not network authority.

## Operational implications

A persistent notification is visible while stay-reachable mode runs. Android/OEM/background policy may still stop the process; the product must present availability honestly.

## Implementation implications

Build a Rust Android runtime adapter plus minimal platform glue for Service/notification/network callbacks. Do not reuse desktop socket keepalive inside the process; service/session lifetime revokes leases directly.

## Revisit conditions

Revisit if Android adds a better first-class persistent P2P/messaging execution primitive, Play policy disallows the selected service category, or process isolation becomes necessary enough to justify an Android Binder/service-process architecture.

## Amendments

Full notes: [`history/0041-amendments.md`](./history/0041-amendments.md).

| Date | Amendment | Effect |
|---|---|---|
| 2026-10-10 | The person's stay-reachable choice is a persisted overlay in the state directory; config.yaml stays the authored default | Decision: `runtime.android.availability_mode` is the authored default and no API rewrites `config.yaml`; the person's Stay-reachable act is a persisted overlay in the profile's state directory under the trust boundary (ADR-0028's trust overlay's sibling), written through one embedded-host call, read as one effective mode by the Service, the derived diagnostic and `background_restart_requires_user_authentication`; off removes the entry; a change restarts the service in the new mode. Not ruled: config as a ceiling. |
| 2026-10-10 | Network access is a runtime permission on API 37+: the Activity asks before binding, the denied state opens the store without a network, a reachable Service posts one notification and ends | Decision: on API 37+ (measured on the Pixel 9a) `INTERNET` is a runtime permission and a fresh install does not hold it; the Activity asks before binding the Service, notifications only after; a denial is the `network_denied` state — the Service opens the store and view hub, starts no runtime, holds no lease, so reading and Keep work as ADR-0044 defines and nothing sends or joins; a reachable Service restarted without the permission posts one notification and ends, and stay reachable requires the notification permission; the embedded runtime is to answer the `EPERM` as a new `EmbeddedRefused::NetworkDenied` (planned). The journey is in human-client-android.md. |
