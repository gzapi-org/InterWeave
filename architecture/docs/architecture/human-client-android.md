# Human client — Android architecture

Status: design, with the shell built (InterWeave #255) and the embedded runtime beneath it (#241, #254, #256).

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

**Where the choice lives (A 2026-10-10, architect-cto's ruling on rust-ui-dev's question).** `runtime.android.availability_mode` in the profile's `config.yaml` is the AUTHORED default (foreground-only when absent) and is never rewritten by the host or the runtime. The person's Stay-reachable act is a PERSISTED OVERLAY in the profile's state directory under the trust boundary — the trust overlay's sibling (ADR-0028 A 2026-10-07), owner-only, the profile's state under ADR-0044 — written through ONE path, the embedded host's API on the lifecycle seam (no IPC method and no minor while no desktop surface has the mode; one added later calls the same function). Everything derived reads ONE effective value, `profile-config`'s effective availability mode — the overlay when present, else the config field — never the field or a platform preference: the Service's start decision, `background_restart_requires_user_authentication`, the diagnostic. Turning the choice off removes the overlay entry (absence is the default, as in trust), never writes `foreground-only` into it; the host signals a change (`Ended::AvailabilityChanged(mode)` from `EmbeddedHost::wait_shutdown_requested`) and the Service restarts in the new mode rather than waiting for the next start. The file is `<state>/availability-overlay.json`, owner-only, holding exactly `{"availability_mode": "stay-reachable"}`; a present overlay that cannot be trusted refuses the host's start, as the trust overlay's does (InterWeave #254). Not ruled: a profile that forbids persistent reachability (config as a ceiling) — today config is a default.

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

## Runtime permissions and the network-denied state (A 2026-10-10)

Android 17 (API 37; measured on the Pixel 9a, google/tegu:17/CP3A.261005.005,
rust-ui-dev's j59) makes `android.permission.INTERNET` a RUNTIME permission
(`prot=dangerous`): a fresh install does not hold it, and with it absent `EmbeddedHost::start`
fails with `EPERM` (measured), which the embedded runtime is to answer as a
new typed `EmbeddedRefused::NetworkDenied` (p2p-network-dev, planned; today
it surfaces as `Internal`). `ACCESS_LOCAL_NETWORK` is a runtime permission there too.
Below API 37 `INTERNET` is granted at install and nothing below applies. The
journey is the client's (rust-ui-dev builds it; the words are
language-culture's):

1. **First launch on API 37+.** The Activity asks for network access BEFORE
   it binds the Service. Notifications are asked only after network is
   granted: network is essential to the product, notifications are optional,
   and a person refuses less when the first ask is the one that matters.
2. **Granted:** bind, start, continue as today.
3. **Denied — the `network_denied` state.** The window shows one state with
   one primary action: it asks again while Android still shows the dialog,
   and otherwise opens the app's settings page; returning to the window
   checks again. **The store opens without a network:** the Service starts in the
   network-denied posture — it opens the profile's store and the view hub,
   starts no runtime and holds no endpoint lease — so nothing is sent or
   joined (no send controls, no join), while reading, Keep and Unkeep work
   exactly as ADR-0044 defines them (a read deletes the unread row unless
   kept; these are LOCAL writes, and the store is not opened read-only).
   Service ownership is unchanged: one Service still owns runtime, store and
   lease; in this posture it owns a store and no runtime, and it is not
   promoted to a foreground service — there is no runtime to keep alive. The
   person's own data is never withheld for want of a network.
4. **Revoked while running** (expected, to verify on the device: Android
   kills the process on a runtime-permission revoke). The next launch is
   step 1 or step 3.
5. **Stay reachable with the permission missing** (a `START_STICKY` restart
   after a revoke — expected, to verify on the device): the Service posts
   ONE notification — network access is off, open the app — and ends the
   foreground service; no retry loop, no silent end. The person opted into
   reachability, and a silent stop would hide the broken promise; the
   notification is the honest signal, and the refusal is logged with its
   cause (a diagnostic field for it is still to be defined). Stay reachable
   therefore REQUIRES the notification permission: its foreground service
   needs a notification in any case, and the switch is offered only while
   `POST_NOTIFICATIONS` is held.
6. **A start answering `NetworkDenied` for any other reason** (a race with
   the grant, another platform restriction) shows state 3.

The `network_denied` state is the ui-model's (human-client-ui.md's
states-not-copy rule); its texts are keys language-culture-en fills, shipped
as marked placeholders until then. "The app will connect when the service
starts", shown today while the start fails, is not one of them.

## Administrative separation

Android has no filesystem admin socket in embedded mode. Instead, the composition root constructs two distinct in-process interfaces:

```text
network/event/UI message path -> LocalDataSession only
explicit settings path        -> LocalAdminPort
```

Remote event handlers never hold `LocalAdminPort`. Trust/config mutations require explicit local UI intent; security-sensitive operations may require Android user presence. This prevents confused-deputy mistakes but is not a sandbox against arbitrary code execution inside the same APK process.

Identity backup/restore remains an offline/stopped-runtime operation. Recovery phrases never traverse `LocalDataSession` or normal message callbacks.

## Android network lifecycle

A small platform bridge observes Android network changes (`ConnectivityManager` callbacks with `LinkProperties`) and hands Rust one thing: `EmbeddedHost::network_changed(NetworkView { addresses })`, a SNAPSHOT of every address of every network the platform reports usable — never a delta, never network ids, interface names or carrier facts (host-specific concepts stop at the bridge, ADR-0001). Non-blocking; the latest view wins; an empty view is offline; before the first view the platform's view is unknown. The Service passes a view when the addresses differ; a reconnect that returns the same addresses is no change at this layer (A 2026-10-09, Stage 17 step 5).

What the runtime then does is `CONNECTIVITY.md` §14's: the view feeds the one network-change detector beside the listeners' bound set; a removal closes the connections that ran from a departed IP, invalidates AutoNAT evidence and takes the relay ladder; an addition lifts the dial gate's peer backoff for every classified peer (infrastructure peers included), makes the data-plane-trusted peers' unclaimed retries due and the relay reservation ladder due — unless the gate still holds the relay peer after the same lift — once per peer per lift floor (the peer schedule's first step, 30 s; the ladder's `retry_min`), every dial still through the normal dial gate (ADR-0011 A 2026-10-09); PeerId, EndpointId, configuration and ADR-0044 retention state are unchanged. An embedded-android profile listens on wildcard addresses only (`ConfigError::AndroidListenerNotWildcard` otherwise): Android names no stable address.

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
- API 37+: network access asked before the Service is bound; notifications asked only after network is granted;
- network denied: the Service opens the store and view hub, starts no runtime, holds no lease, is not a foreground service; read/Keep/Unkeep work, nothing sends or joins;
- network revoked while running: process death, then step 1 or 3 on the next launch;
- stay reachable restarted without network access: one notification, then the foreground service ends; no retry;
- a start answering `NetworkDenied`: the `network_denied` state;
- stay reachable cannot be enabled while `POST_NOTIFICATIONS` is not held.

## Android backup / transfer boundary

The Android platform backup system is not part of the application's disaster-recovery design. Standard v1 excludes the wrapped identity envelope, transport/trust configuration, recovery temporary state, and human SQLite database from both cloud backup and device-to-device extraction, and packages explicit backup/data-extraction rules rather than relying on platform defaults. A new installation or device transfer that lacks the valid local Keystore-wrapped identity enters unconfigured/recovery-required onboarding; it never manufactures a replacement PeerId for an established profile.

Human message backup/synchronization is disabled in standard v1 system backup. A future user-selected encrypted application backup may include message content only from `unread_inbound` and `kept_inbound`; `pending_outbound`, transport-terminal outbound, and read-unkept inbound are excluded. Cross-device history/sync remains a separate application protocol/service decision and does not relax the transport's no-central-store/no-offline-mailbox claims.

## Availability-policy interaction

`stay-reachable + user-presence` is intentionally allowed but self-limiting. While the unlocked foreground service remains alive it may stay online. After service/process restart it cannot unwrap the identity until the user authenticates; local status must expose `background_restart_requires_user_authentication=true`, and UI/notifications must not claim automatic post-restart reachability.
