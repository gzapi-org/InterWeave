# First-party human clients use shared Rust core and Slint UI

**Status:** Accepted

## Context

The architecture now targets both desktop and Android human-facing clients. A first-party implementation should maximize reuse without coupling transport contracts to one GUI toolkit or requiring separate application logic stacks per platform.

## Decision

Use Rust for first-party human-client domain logic, application protocol, storage, transport adapters, and UI view models. Use Slint as the reference first-party GUI for Windows/macOS/Linux and Android. Keep Slint and human application models above transport contracts; IPC and network protocols remain language-neutral.

Android may contain a minimal Java/Kotlin/JNI platform shim only for Android component/lifecycle/notification/Keystore interfaces that require JVM-facing APIs. No application routing, trust, crypto policy, message parsing, or persistence business logic lives there.

## Alternatives considered

Separate Kotlin Android and Rust desktop apps; Tauri/web UI; egui; native platform UI per OS; require every future client to be Rust.

## Consequences

Most UI/application logic and fixtures can be shared. Platform layouts and lifecycle integration remain platform-specific. A future third-party client can still implement the public protocols in another language.

## Security implications

One shared Rust validation/domain layer reduces divergent security behavior. UI toolkit labels/events never become transport authority. Accessibility and platform integration are release-tested rather than assumed.

## Operational implications

Build/release pipelines need desktop Rust targets plus Android NDK/Rust targets and minimal Android packaging glue.

## Implementation implications

Add `human-core`, `human-chat-protocol`, `human-store`, `human-ui-model`, `human-ui-slint`, `human-desktop`, and `human-android` crates in the future workspace. No production crates are created in this architecture repository.

## Revisit conditions

Revisit Slint if accessibility, platform integration, performance, licensing, or mobile-store requirements fail release criteria. Do not revisit the Rust/shared-core boundary merely because the presentation toolkit changes.
