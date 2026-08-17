# Human client — platform packaging and lifecycle matrix

Status: architecture/design only. Packaging technology may evolve without changing transport/application contracts.

## 1. Shared release contents

Every first-party release is built from the same Rust human-core/chat/store/UI-model crates and the frozen network/application fixtures. Platform packages differ only at the deployment/lifecycle boundary.

| Platform | Runtime ownership | Local binding | Background model | Key custody |
|---|---|---|---|---|
| Windows | external profile daemon | IPC v2 named pipes | user-started/autostart daemon policy | normal profile identity; ADR-0038 future encrypted option |
| macOS | external profile daemon | IPC v2 Unix sockets | user-started/LaunchAgent-style policy | normal profile identity; ADR-0038 future encrypted option |
| Linux | external profile daemon | IPC v2 Unix sockets | user-started/systemd-user-style policy | normal profile identity; ADR-0038 future encrypted option |
| Android | in-process TransportRuntime hosted by app service | `LOCAL-CLIENT` adapter | foreground-only or explicit stay-reachable foreground service | AndroidKeyStore AES-GCM wrapping of exact Ed25519 seed |

No platform package changes DirectMessageV2, GossipSub, EndpointId, Kademlia, AutoNAT/Relay/DCUtR, trust, or delivery semantics.

## 2. Desktop process layout

A desktop installation contains three executable roles even if one installer bundles them:

```text
human-desktop     Rust/Slint application
transport-daemon  profile network runtime
transportctl      admin/offline identity/recovery CLI
```

The desktop launcher may start the daemon if absent, but the GUI does not own daemon shutdown. Autostart is an explicit user/operator preference. The daemon and GUI update independently only when IPC-contract compatibility permits it.

## 3. Windows

Use the Windows named-pipe binding defined by IPC v2. Run the daemon in the user security context by default; avoid requiring administrator privileges merely for messaging. Installation/update signing, autostart registration, firewall prompts/rules, and crash-restart behavior require platform release tests.

The admin named pipe remains a distinct authority domain from the data pipe. A future Windows-specific stronger same-user authorization mechanism is evaluated under SPIKE-005 rather than encoded into `client.kind`.

## 4. macOS

Use owner-protected Unix-domain sockets in the profile runtime directory. The desktop app is a normal signed application bundle; persistent daemon startup is an explicit user-level background policy, not implied by opening a chat window once.

Any future Keychain/Secure Enclave identity backend must preserve the identity/recovery ADR or explicitly supersede it. It is not introduced implicitly by packaging.

## 5. Linux

Use owner-protected Unix-domain sockets. A user service manager such as a systemd user unit may manage the daemon where available, but the architecture does not require systemd and does not require root. Desktop-environment notifications and autostart are adapters above the Rust human core.

Sandboxed packaging formats require explicit access to the profile IPC/runtime directories; do not weaken socket permissions globally to make a sandbox work.

## 6. Android

The Android package contains one application identity/process family. The foreground service hosts the Rust TransportRuntime; the Activity/Slint UI is a presentation client of the service-owned local session.

Stay-reachable packaging must declare and satisfy the Android foreground-service type/permission/store-policy requirements validated by SPIKE-008. If policy validation fails, the release must honestly fall back to foreground-only behavior or supersede ADR-0041; it must not silently introduce centralized push infrastructure.

Supported ABI/minimum/target API choices are release parameters, not architecture constants, but every shipped ABI must run the same wire/crypto conformance fixtures.

### Android backup and device-transfer manifest policy

Standard v1 does not rely on Android system backup for identity, configuration or human message state. The package must:

- set `android:allowBackup="false"` explicitly rather than inheriting the platform default;
- provide Android 12+ `android:dataExtractionRules` that exclude the wrapped identity envelope, expected PeerId/wrapping metadata, transport/trust configuration, recovery temporary state and human-store database from **both** `<cloud-backup>` and `<device-transfer>`; `allowBackup=false` is not relied on as the only device-transfer control;
- provide the corresponding Android 11-and-lower `android:fullBackupContent` exclusion rules for supported older devices;
- declare the dedicated recovery Activity non-exported and `android:excludeFromRecents="true"`; set `FLAG_SECURE` before rendering recovery material;
- treat the no-backup/cache directories as implementation aids, not the sole policy control;
- never interpret a system/device-transfer restore without a valid local Keystore key as an identity restore.

The entire human-store database is excluded from Android system backup/transfer in standard v1. A future explicit encrypted application backup may include **message content only from inbound unread and receiver-kept records**. Pending outbound is deliberately excluded from portable backup to avoid restored/second-device replay; transport-terminal outbound and read-unkept inbound are not durable to begin with. Any broader history/sync requires a separate application-security/replay design rather than a packaging toggle.

## 7. Update compatibility

Before activating an update:

- app DB migrations are transactional and independently recoverable;
- transport config migrations are validated before daemon/runtime startup;
- identity key formats are never rewritten without a versioned/atomic migration;
- desktop human UI and daemon negotiate IPC major/minor compatibility;
- Android bundled UI/runtime are version-aligned in one package;
- wire protocol compatibility remains governed by the frozen network protocols rather than app package version.

Rollback must never silently generate a new PeerId.

## 8. Crash and recovery behavior

Desktop GUI crash -> daemon may remain online, human EndpointId lease is released when IPC death is detected.

Desktop daemon crash -> all profile endpoints go offline until daemon restart; the separate human app retains only pending outbound, unread inbound, and receiver-kept inbound per ADR-0044.

Android Activity crash/recreation -> service/runtime may remain online.

Android service/process death -> transport endpoint is offline; restart reconstructs ephemeral network state with the same unlocked PeerId when policy permits.

No platform converts crashes into a durable **transport** inbox. Human application survival is limited to the ADR-0044 retention sets and never creates remote offline acceptance.

## 9. Release matrix

A release candidate tests at least:

- Windows/macOS/Linux install, upgrade, rollback, daemon start/stop/autostart and IPC ACLs;
- Android install/upgrade/process death/background/foreground/network change and Keystore behavior;
- Android secure recovery screens, in-app mnemonic picker/no-clipboard policy, backup/data-extraction exclusions, and device-to-device reinstall/restore behavior;
- one desktop human + Claude shared profile routing;
- desktop-to-Android and Android-to-desktop HumanChatV2 over direct and relay paths;
- distinct-device PeerIds and contact grouping;
- identical wire/golden fixtures on every shipped target;
- recovery drill without accidental identity regeneration;
- no production package introduces a hidden centralized relay/push/account dependency outside configured libp2p connectivity infrastructure.
