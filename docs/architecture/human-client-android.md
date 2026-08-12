# Human client — Android architecture

Status: architecture/design only.

## Deployment decision

Android does **not** run the desktop standalone daemon/UDS model. The APK hosts the same Rust `TransportRuntime` inside a user-visible Android foreground service when continuous reachability is enabled:

```text
Android APK
+-------------------------------------------------------+
| Slint Activity / Rust UI                              |
| human-core / human-store                              |
|              |                                        |
|       LocalDataSession adapter                        |
|              |                                        |
|     Rust TransportRuntime                             |
|              |                                        |
|  Android foreground Service host                      |
|  (minimal platform glue around Rust runtime)          |
+--------------|----------------------------------------+
               v
             libp2p
```

There is no localhost TCP daemon fallback and no cross-app IPC surface in Android v1.

## Foreground/background modes

The human app exposes two user-visible modes:

### Foreground-only

Runtime is active while the app/service is intentionally running in foreground use. When stopped/background policy tears it down, endpoint `human` becomes offline and remote directs receive `no_route`.

### Stay reachable

User explicitly enables persistent P2P reachability. While active, Android hosts the Rust runtime in a foreground service with a persistent notification. For the first-party messaging use case, the selected current Android service category is `remoteMessaging`; target-SDK/Play-policy compatibility is a release gate because Android service policy can change.

Do **not** classify the persistent socket runtime as `dataSync`: current Android places time limits on that service class. Do not silently fall back to `specialUse` without a reviewed platform-policy change.

The app must start continuous reachability from a user-visible interaction or another Android-permitted start path. It does not claim it can resurrect an arbitrary foreground service from every background state, reboot, force-stop, or OEM power policy.

## Service ownership and local session

The foreground service owns:

- Rust Tokio runtime / `TransportRuntime`;
- profile key unlock lifetime;
- listener/Swarm;
- EndpointRegistry;
- local `human` data session and exclusive lease;
- connectivity/discovery timers;
- notification-facing normalized state.

Activity destruction/rotation does not release the endpoint lease while the service stays alive. Service termination releases it immediately. A new service instance creates a fresh lease epoch and rebuilds ephemeral discovery/connectivity state.

The Activity/view model never supplies `source_endpoint`; it sends through the service-owned `LocalDataSession`.

## Administrative separation

Android has no filesystem admin socket in embedded mode. Instead, the composition root constructs two distinct in-process interfaces:

```text
network/event/UI message path -> LocalDataSession only
explicit settings path        -> LocalAdminPort
```

Remote event handlers never hold `LocalAdminPort`. Trust/config mutations require explicit local UI intent; security-sensitive operations may require Android user presence. This prevents confused-deputy mistakes but is not a sandbox against arbitrary code execution inside the same APK process.

Identity backup/restore remains an offline/stopped-runtime operation. Recovery phrases never traverse `LocalDataSession` or normal message callbacks.

## Android network lifecycle

A small platform bridge observes Android network changes and emits only normalized local events to Rust:

```text
available / lost / capabilities changed / link changed
```

On a material network change the Rust runtime:

1. invalidates affected AutoNAT evidence;
2. removes stale relay-derived assumptions and reconciles reservations;
3. re-evaluates listener/address registry;
4. restarts/rebinds mDNS as applicable;
5. marks old address candidates stale without changing PeerId;
6. reconnects trusted peers through the normal DialAdmissionGate;
7. leaves EndpointId/config and ADR-0044 retention state unchanged.

## Android discovery/resource profile

- Kademlia remains standard-v1 enabled but **client mode only** on the phone; no Kademlia server role.
- AutoNAT client, Relay client, and DCUtR remain mandatory while the runtime is active; relay/probe server roles are disabled.
- mDNS is optional and should default off for always-background mode. If enabled on Wi-Fi, the platform adapter acquires multicast capability/lock only while the mDNS provider actually needs it and releases it promptly; never hold multicast solely to keep the app alive.
- timers/query intensity may use an Android/mobile profile within already frozen ceilings, but battery tuning must not weaken trust, validation, or wire semantics.

## Process death and offline semantics

Android can kill application/service processes. The architecture therefore promises:

- no hidden daemon survival;
- no transport offline mailbox;
- no guaranteed reception while the service is absent;
- clean reconstruction of PeerId/runtime state when the app can restart;
- remote direct send to absent `human` -> `no_route` once peer route state reflects absence/unreachability;
- local human database contains message content only in ADR-0044 states: pending outbound, unread inbound, and receiver-kept-after-read inbound.

A future centralized push wake-up service (for example FCM) would introduce a new infrastructure/privacy dependency and requires a separate ADR. It is not part of standard v1.

## Notifications

When the human application consumes an inbound message while the foreground service is active but Activity is not visible, it first commits the message as `unread_inbound` under ADR-0044, then may post a local Android notification. Notification previews are user-configurable because application payloads may be sensitive. Tapping the notification opens the local conversation but does not by itself force Keep; notification content never executes transport/admin commands. A notification must not become a shadow durable message archive after the application deletes content.

## UI

Slint is the reference Rust UI. Android-specific layout must account for touch targets, safe areas, virtual keyboard, accessibility labels/actions, lifecycle restoration, and narrow-screen navigation. Shared components may be reused from desktop, but mobile navigation is not forced into a desktop window model.

## Acceptance matrix

Test at minimum:

- Activity recreated while service remains online;
- service stop releases endpoint lease;
- process death/restart rebuilds relay/Kademlia state with same PeerId;
- foreground-only mode correctly goes offline;
- stay-reachable mode shows persistent notification and honors OS start restrictions;
- Wi-Fi <-> cellular switch invalidates/rebuilds reachability;
- relay fallback under carrier-NAT-like topology;
- DCUtR success/failure with relay preserved;
- Android mDNS permission/multicast behavior on supported API ranges;
- Keystore unlock modes and process restart;
- no admin access reachable from message callback graph;
- same wire fixtures as desktop.

## Android backup / transfer boundary

The Android platform backup system is not part of the application's disaster-recovery design. Standard v1 excludes the wrapped identity envelope, transport/trust configuration, recovery temporary state, and human SQLite database from both cloud backup and device-to-device extraction, and packages explicit backup/data-extraction rules rather than relying on platform defaults. A new installation or device transfer that lacks the valid local Keystore-wrapped identity enters unconfigured/recovery-required onboarding; it never manufactures a replacement PeerId for an established profile.

Human message backup/synchronization is disabled in standard v1 system backup. A future user-selected encrypted application backup may include message content only from `unread_inbound` and `kept_inbound`; `pending_outbound`, transport-terminal outbound, and read-unkept inbound are excluded. Cross-device history/sync remains a separate application protocol/service decision and does not relax the transport's no-central-store/no-offline-mailbox claims.

## Availability-policy interaction

`stay-reachable + user-presence` is intentionally allowed but self-limiting. While the unlocked foreground service remains alive it may stay online. After service/process restart it cannot unwrap the identity until the user authenticates; local status must expose `background_restart_requires_user_authentication=true`, and UI/notifications must not claim automatic post-restart reachability.
