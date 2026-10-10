// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The host stand-in behind the device seam: it serves under its app data
//! directory as the shipped Android example configures it, and a process
//! death and restart bring the same `PeerId` back with the profile free.

#![allow(clippy::expect_used, clippy::panic)]
#![cfg(target_os = "linux")]

use interweave_android_e2e_tests::{Device as _, HostStandIn};

/// The shipped `human-android.yaml`, with every placeholder a peer that
/// does not exist and its listener on loopback.
fn shipped() -> String {
    let raw = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../architecture/config/examples/human-android.yaml"),
    )
    .expect("the example is readable");
    let stranger = interweave_profile_identity::ProfileIdentity::generate()
        .transport_identity()
        .expect("a peer");
    raw.replace("<PEER_A>", stranger.as_str())
        .replace("<INFRA_A>", stranger.as_str())
        .replace("<INFRA_B>", stranger.as_str())
        .replace("/ip4/0.0.0.0/tcp/0", "/ip4/127.0.0.1/tcp/0")
}

/// A kill and a restart: the same `PeerId`, a bound listener, and the
/// profile's lock released by the death -- the restart would be refused
/// `LockHeld` otherwise. The control is the first start, serving under
/// the runtime root the host made in the app data directory.
#[test]
fn a_killed_stand_in_restarts_as_the_same_peer() {
    let mut device = HostStandIn::start(&shipped());
    let peer = device.peer();
    assert_eq!(
        device.host().paths().boundary().runtime_root(),
        Some(device.app_data_dir().join("interweave").as_path()),
        "the runtime root sits in the app data directory"
    );
    let before = device.listening();
    assert_eq!(before.len(), 1, "{before:?}");

    device.kill();
    device.restart();
    assert_eq!(device.peer(), peer, "the same profile identity");
    let after = device.listening();
    assert_eq!(after.len(), 1, "{after:?}");
    assert!(after[0].starts_with("/ip4/127.0.0.1/tcp/"), "{after:?}");
    device.stop();
}
