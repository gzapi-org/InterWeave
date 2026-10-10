// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The host stand-in behind the device seam: it serves under its app data
//! directory as the shipped Android example configures it, and a process
//! death and restart bring the same `PeerId` back with the profile free.

#![allow(clippy::expect_used, clippy::panic)]
#![cfg(target_os = "linux")]

use std::collections::BTreeSet;

use interweave_android_e2e_tests::{Device as _, HostStandIn};
use interweave_local_client_api::{AdminBinding as _, AdminCapability, AdminPort as _};
use interweave_transport_api::TransportIdentity;

/// The `PeerId` the running runtime serves as, read over its admin port --
/// not the stand-in's record of what it launched.
fn serving(device: &HostStandIn) -> TransportIdentity {
    device.host().runtime().block_on(async {
        let port = device
            .binding()
            .admin(BTreeSet::from([AdminCapability::Status]))
            .await
            .expect("an admin port");
        port.status().await.expect("status").peer
    })
}

/// The shipped `human-android.yaml`, with every placeholder a peer that
/// does not exist; it listens on the wildcard, as the device does.
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
}

/// A kill and a restart: the same `PeerId`, a bound listener, and the
/// profile's lock released by the death -- the restart would be refused
/// `LockHeld` otherwise. The control is the first start, serving under
/// the runtime root the host made in the app data directory.
#[test]
fn a_killed_stand_in_restarts_as_the_same_peer() {
    let mut device = HostStandIn::new();
    device.start(&shipped());
    let peer = device.peer();
    assert_eq!(
        serving(&device),
        peer,
        "the control: the first start serves it"
    );
    assert_eq!(
        device.host().paths().boundary().runtime_root(),
        Some(device.app_data_dir().join("interweave").as_path()),
        "the runtime root sits in the app data directory"
    );
    let before = device.listening();
    assert_eq!(before.len(), 1, "{before:?}");

    device.kill();
    device.restart();
    assert_eq!(
        serving(&device),
        peer,
        "the runtime serves the same identity"
    );
    let after = device.listening();
    assert_eq!(after.len(), 1, "{after:?}");
    assert!(!after[0].ends_with("/tcp/0"), "a bound port: {after:?}");
    device.stop();
}
