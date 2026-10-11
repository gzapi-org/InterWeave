// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Plan §20 gate (g) on the Android side, through the device seam (gate
//! (d) is `platform_gates.rs`, and the header there is this file's too).
//!
//! Its own test binary, as `audit_sink.rs` in the embedded crate is: the
//! stand-in's log is a GLOBAL subscriber whose filter follows the running
//! host's level, so a second stand-in in the same process, at another
//! level, would move the filter under this case.
//!
//! - (d) `trust_boundary`: the runtime root directly under the app data
//!   directory, it and the profile's private directories `0700`, and a
//!   private directory under the platform's `files/` refused by the
//!   runtime root while the ancestor walk alone accepts it (the control).
//! - (g) `audit`: with the profile's log level at WARN, stricter than
//!   INFO, a trust set made through the admin port appears in the Android
//!   side's log as the audit record `admin.trust.set`.
//!
//! On the stand-in, `files/` is made `0771` as Android makes it, and the
//! log is `log_capture`'s: the embedded host's own filter in a test
//! subscriber, standing where the app's logcat writer stands. On a device
//! (`#[ignore]`d, `src/adb.rs`) both are the platform's: the app's real
//! `files/` and logcat. What this does NOT prove: the human store opening
//! under the boundary (the app's, with the store), and (g)'s logcat writer
//! on the stand-in -- only the filter it is handed.

#![cfg(target_os = "linux")]
#![allow(clippy::expect_used, clippy::panic)]

use std::time::Duration;

use interweave_android_e2e_cases::cases;
use interweave_android_e2e_tests::adb::AdbDevice;
use interweave_android_e2e_tests::{Device, HostStandIn, PATIENCE, log_capture};
use interweave_profile_identity::ProfileIdentity;
use interweave_test_support::e2e;

/// The shipped Android example, listening on a wildcard as an
/// `embedded-android` profile must, at `level`.
fn config(level: &str) -> String {
    let stranger = ProfileIdentity::generate()
        .transport_identity()
        .expect("a peer");
    let raw = e2e::example("human-android.yaml", &stranger, "/ip4/0.0.0.0/tcp/0", None);
    assert_eq!(
        raw.matches("observability: { log_level: debug }").count(),
        1,
        "the harness writes one log level"
    );
    raw.replace(
        "observability: { log_level: debug }",
        &format!("observability: {{ log_level: {level} }}"),
    )
}

/// The audit record, by the message the runtime gives it.
const AUDIT_RECORD: &str = "admin.trust.set";

fn audit<A: Device>(mut android: A) {
    android.start(&config("warn"));
    let peer = ProfileIdentity::generate()
        .transport_identity()
        .expect("a peer");
    let _ = android
        .run_case(cases::AUDIT, &serde_json::json!({ "peer": peer.as_str() }))
        .passed(|| android.log());
    let deadline = std::time::Instant::now() + PATIENCE;
    while !android.log().contains(AUDIT_RECORD) {
        assert!(
            std::time::Instant::now() < deadline,
            "the trust set's audit record never reached the Android side's log at WARN:\n{}",
            android.log()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    android.stop();
}

#[test]
fn gate_g_on_the_stand_in_a_trust_set_reaches_a_log_filtered_at_warn() {
    log_capture::install();
    audit(HostStandIn::new());
    // The control: the filter is live. At WARN it let the audit record
    // through and nothing else below WARN -- a run at DEBUG captures the
    // runtime's DEBUG lines (`interweave::connectivity`), so a filter that
    // admitted more than WARN would show them here.
    let lines = log_capture::lines();
    let below_warn: Vec<&str> = lines
        .lines()
        .filter(|l| {
            ["TRACE ", "DEBUG ", "INFO "]
                .iter()
                .any(|level| l.starts_with(level))
                && !l.contains("interweave::audit")
        })
        .collect();
    assert!(
        below_warn.is_empty(),
        "lines below WARN passed a WARN filter: {below_warn:?}"
    );
}

#[test]
#[ignore = "needs a device with the app's androidTest build: ANDROID_SERIAL and INTERWEAVE_ANDROID_INSTRUMENTATION (src/adb.rs); one phone, so --test-threads 1"]
fn gate_g_on_a_device_a_trust_set_reaches_logcat_filtered_at_warn() {
    audit(AdbDevice::connect());
}
