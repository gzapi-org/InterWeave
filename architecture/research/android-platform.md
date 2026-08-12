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

## Recovery-screen privacy

Android `FLAG_SECURE` marks a window as secure so screenshots/non-secure-display capture are blocked by the platform policy. A dedicated recovery task can also be excluded from Recents with `android:excludeFromRecents="true"`. For any IME-assisted filtering, `IME_FLAG_NO_PERSONALIZED_LEARNING` requests that the IME not update personalized typing history, but Android explicitly documents that an IME may ignore the request. The architecture therefore avoids normal free-text mnemonic entry and uses an in-app word picker; the IME flag is only defense in depth.

Sources:
- https://developer.android.com/reference/android/view/WindowManager.LayoutParams#FLAG_SECURE
- https://developer.android.com/guide/components/activities/recents
- https://developer.android.com/reference/android/view/inputmethod/EditorInfo#IME_FLAG_NO_PERSONALIZED_LEARNING

## Backup and device transfer

Android Auto Backup is enabled by default for eligible apps unless the manifest changes the posture. Android 12+ uses `data-extraction-rules` with separate cloud-backup and device-transfer sections; Android documentation warns that `allowBackup=false` does not uniformly disable device-to-device transfer on every manufacturer implementation. Sensitive identity/configuration and human-history state therefore receive explicit extraction exclusions in addition to `allowBackup=false`, and supported pre-Android-12 devices retain the older full-backup rules.

Sources:
- https://developer.android.com/identity/data/autobackup
- https://developer.android.com/privacy-and-security/risks/backup-best-practices
- https://developer.android.com/about/versions/12/behavior-changes-12#backup-restore
