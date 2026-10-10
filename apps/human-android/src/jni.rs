// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The entry points the Kotlin side calls (`Native.kt`): the network
//! service's start, its wait for the end, its stop, the notices it posts,
//! and the texts it shows. Each catches a panic at the boundary
//! (`EnvUnowned::with_env`) and answers Java with a value, or a
//! `RuntimeException` on a JNI failure, never an unwind.

use std::ffi::CString;
use std::time::Duration;

use interweave_human_android_platform::AvailabilityMode;
use interweave_human_android_platform::{Ended, ServiceHost, ServiceLaunch, StartRefused};
use interweave_human_ui_model::{UiText, fill, placeholder_en};
use interweave_profile_identity::ProfileIdentity;
use jni::EnvUnowned;
use jni::errors::ThrowRuntimeExAndDefault;
use jni::objects::{JClass, JString};
use jni::sys::{jboolean, jint, jlong};

/// The profile the app runs, as the android-e2e harness names it.
const PROFILE: &str = "human-android";

/// How long a stop lets exchanges in flight settle: what the runtime's
/// own tests use, and p2p-network-dev-01's word that it is enough (relay
/// 01a12498).
const GRACE: Duration = Duration::from_secs(1);

/// `Native.start`'s refusals, negative so they never read as a mode.
mod refused {
    use super::jint;
    pub(super) const NO_IDENTITY: jint = -1;
    #[cfg_attr(
        not(feature = "dev-stand-ins"),
        allow(dead_code, reason = "only the stand-in writes a profile today")
    )]
    pub(super) const PROFILE: jint = -2;
    pub(super) const RUNTIME: jint = -3;
    pub(super) const ENDPOINT: jint = -4;
    pub(super) const STORE_RECOVERY: jint = -5;
    pub(super) const STORE_PRIVATE: jint = -6;
    pub(super) const STORE_UNAVAILABLE: jint = -7;
    pub(super) const THREAD: jint = -8;
}

/// One line to the platform's log, under the app's tag. A started
/// Service has no Activity, so nothing has redirected stderr there.
pub(crate) fn log(line: &str) {
    let (Ok(tag), Ok(text)) = (
        CString::new("interweave"),
        CString::new(line.replace('\0', " ")),
    ) else {
        return;
    };
    #[allow(
        unsafe_code,
        reason = "the NDK's logging call: two valid C strings, read during the call only"
    )]
    // SAFETY: both pointers are to NUL-terminated strings that outlive the call.
    unsafe {
        ndk_sys::__android_log_write(
            i32::try_from(ndk_sys::android_LogPriority::ANDROID_LOG_INFO.0).unwrap_or(4),
            tag.as_ptr(),
            text.as_ptr(),
        );
    }
}

/// The identity the runtime starts with. Until step 6's Keystore key
/// lands, the debug build's stand-in; without it, none.
#[allow(
    clippy::unnecessary_wraps,
    reason = "None in a build without dev-stand-ins, which has no identity source yet"
)]
fn identity() -> Option<ProfileIdentity> {
    #[cfg(feature = "dev-stand-ins")]
    {
        Some(interweave_human_android_platform::stand_in::identity())
    }
    #[cfg(not(feature = "dev-stand-ins"))]
    {
        None
    }
}

fn start(app_data_dir: String) -> jint {
    let Some(identity) = identity() else {
        log("no identity source in this build: the network service does not start");
        return refused::NO_IDENTITY;
    };
    #[cfg(feature = "dev-stand-ins")]
    if let Err(e) = interweave_human_android_platform::stand_in::provision(
        std::path::Path::new(&app_data_dir),
        PROFILE,
    ) {
        log(&format!("the stand-in profile could not be written: {e}"));
        return refused::PROFILE;
    }
    let host = ServiceHost::global();
    match host.start(ServiceLaunch {
        app_data_dir: app_data_dir.into(),
        profile: PROFILE.to_owned(),
        identity,
    }) {
        Ok(()) => match host.availability() {
            Some(AvailabilityMode::StayReachable) => 1,
            _ => 0,
        },
        Err(why) => {
            log(&format!("the network service did not start: {why:?}"));
            match why {
                StartRefused::Runtime(_) => refused::RUNTIME,
                StartRefused::NoHumanEndpoint => refused::ENDPOINT,
                StartRefused::StoreNeedsRecovery => refused::STORE_RECOVERY,
                StartRefused::StoreNotPrivate(_) => refused::STORE_PRIVATE,
                StartRefused::StoreUnavailable(_) => refused::STORE_UNAVAILABLE,
                StartRefused::Thread(_) => refused::THREAD,
            }
        }
    }
}

/// `Native.start(appDataDir)`.
#[allow(
    unsafe_code,
    reason = "the JVM finds a native method by its unmangled name"
)]
#[unsafe(no_mangle)]
extern "system" fn Java_org_interweave_human_Native_start<'caller>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    app_data_dir: JString<'caller>,
) -> jint {
    env.with_env(|env| -> Result<jint, jni::errors::Error> {
        Ok(start(app_data_dir.try_to_string(env)?))
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `Native.waitEnded()`.
#[allow(
    unsafe_code,
    reason = "the JVM finds a native method by its unmangled name"
)]
#[unsafe(no_mangle)]
extern "system" fn Java_org_interweave_human_Native_waitEnded<'caller>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
) -> jint {
    env.with_env(|_| -> Result<jint, jni::errors::Error> {
        Ok(match ServiceHost::global().wait_ended() {
            Ended::ShutdownRequested { .. } => 1,
            Ended::NotRunning => 0,
        })
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `Native.stop()`.
#[allow(
    unsafe_code,
    reason = "the JVM finds a native method by its unmangled name"
)]
#[unsafe(no_mangle)]
extern "system" fn Java_org_interweave_human_Native_stop<'caller>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
) -> jboolean {
    env.with_env(|_| -> Result<jboolean, jni::errors::Error> {
        let Some(stopped) = ServiceHost::global().stop(GRACE) else {
            return Ok(false);
        };
        log(&format!(
            "stopped: session closed {}, runtime {:?}",
            stopped.session_closed, stopped.runtime
        ));
        Ok(true)
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `Native.waitNotice(timeoutMs)`.
#[allow(
    unsafe_code,
    reason = "the JVM finds a native method by its unmangled name"
)]
#[unsafe(no_mangle)]
extern "system" fn Java_org_interweave_human_Native_waitNotice<'caller>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    timeout_ms: jlong,
) -> jint {
    env.with_env(|_| -> Result<jint, jni::errors::Error> {
        let timeout = Duration::from_millis(u64::try_from(timeout_ms).unwrap_or(0));
        Ok(ServiceHost::global()
            .hub()
            .notices()
            .wait_change(timeout)
            .map_or(-1, |count| jint::try_from(count).unwrap_or(jint::MAX)))
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// The text `Native.text(key, count)` answers, by `Text.kt`'s keys.
fn text(key: jint, count: jint) -> String {
    let key = match key {
        0 => UiText::ReachableChannel,
        1 => UiText::ReachableTitle,
        2 => UiText::ReachableBody,
        3 => UiText::MessagesChannel,
        _ if count == 1 => UiText::NewMessage,
        _ => UiText::NewMessages,
    };
    fill(placeholder_en::text(key), &[("count", &count.to_string())])
}

/// `Native.text(key, count)`.
#[allow(
    unsafe_code,
    reason = "the JVM finds a native method by its unmangled name"
)]
#[unsafe(no_mangle)]
extern "system" fn Java_org_interweave_human_Native_text<'caller>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    key: jint,
    count: jint,
) -> JString<'caller> {
    env.with_env(|env| -> Result<JString<'caller>, jni::errors::Error> {
        JString::from_str(env, text(key, count))
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}
