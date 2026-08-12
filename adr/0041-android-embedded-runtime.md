# Android human client embeds TransportRuntime in a foreground service

**Status:** Accepted; platform-specific amendment to ADR-0015/0032/0037

## Context

The desktop standalone-daemon model maps poorly to Android background/process rules. Android nevertheless needs the same PeerId/EndpointId/network semantics and, when the user asks to remain reachable, a user-visible lifecycle owner independent of the Activity window.

## Decision

The Android first-party app embeds the Rust `TransportRuntime` inside an Android foreground service host rather than launching a standalone daemon or exposing local TCP/UDS IPC. The UI talks to the runtime through the neutral `LOCAL-CLIENT` in-process adapter. The service owns the `human` EndpointId lease while active.

Continuous background reachability is explicit user opt-in and uses the current Android `remoteMessaging` foreground-service category subject to SPIKE-008 target-SDK/Play-policy validation. Foreground-only mode is supported. No centralized push wake-up dependency is added.

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
