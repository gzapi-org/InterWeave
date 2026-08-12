# Android platform findings for the Rust human client

Checked: 2026-08-12. Primary sources only.

## UI / Rust

Slint supports Rust applications on Android through its Android backend and `android-activity`, and supports Windows/macOS/Linux desktops. This makes it a viable first-party shared UI technology while keeping transport protocols language-neutral.

Sources:
- https://docs.slint.dev/latest/docs/slint/guide/platforms/mobile/android/
- https://docs.slint.dev/latest/docs/slint/guide/platforms/desktop/
- https://docs.slint.dev/latest/docs/slint/reference/common/

## Foreground services

Current Android requires foreground-service types for modern target SDKs. `remoteMessaging` exists for device-to-device messaging use cases; foreground services remain user-visible through a notification and background starts are restricted. `dataSync` has background execution time limits on Android 15+, so it is not a suitable category for an indefinitely reachable P2P messaging runtime.

Sources:
- https://developer.android.com/develop/background-work/services/fgs/service-types
- https://developer.android.com/develop/background-work/services/fgs/launch
- https://developer.android.com/develop/background-work/services/fgs/restrictions-bg-start
- https://developer.android.com/develop/background-work/services/fgs/timeout

The architecture therefore treats Android background reachability as explicit user-visible service operation, not guaranteed immortal execution. SPIKE-008 validates the selected FGS category and current Play/target-SDK policy before release.

## Keystore

Android Keystore supports non-exportable keys and may bind supported keys to TEE/StrongBox. The transport Ed25519 seed must remain portable for exact PeerId recovery, so the selected architecture uses a Keystore-held AES wrapping key to protect the portable Ed25519 secret at rest rather than pretending Android supplies a non-exportable libp2p Ed25519 identity primitive.

Source:
- https://developer.android.com/privacy-and-security/keystore

## mDNS / multicast

Android Wi-Fi may filter multicast; `WifiManager.MulticastLock` enables multicast reception and has battery cost. Current Android NSD documentation also notes OS-version-dependent multicast handling and newer local-network permission requirements. The mobile adapter therefore acquires multicast support only while mDNS is enabled/needed and never treats it as a keepalive mechanism.

Sources:
- https://developer.android.com/reference/android/net/wifi/WifiManager.MulticastLock
- https://developer.android.com/reference/android/net/nsd/NsdManager
