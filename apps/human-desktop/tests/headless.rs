// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The window's side of the root, headless: the real view on Slint's
//! testing backend, the facade on its own thread over the fake network,
//! a real store. What it does NOT prove: a real daemon, a real window, a
//! platform's focus and event loop -- the desktop end-to-end suite's and
//! batch 4's.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use interweave_human_app_core::{Command, FacadeSide, Opener, Update};
use interweave_human_desktop::app::{App, SlintSurface};
use interweave_human_desktop::facade_thread::FacadeThread;
use interweave_human_store::{HumanStore, StoreOptions};
use interweave_human_transport_client::{ClientConfig, TransportClient};
use interweave_human_ui_model::{ConversationKey, SessionNotice};
use interweave_human_ui_slint::View;
use interweave_local_client_fake::{FakeConfig, FakeEndpoint, FakeNetwork, FakeNode};
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::EndpointId;

fn human() -> EndpointId {
    EndpointId::parse("human").expect("endpoint")
}

fn node() -> FakeConfig {
    FakeConfig {
        peer: ProfileIdentity::generate()
            .transport_identity()
            .expect("peer"),
        endpoints: vec![FakeEndpoint::open(human(), false)],
        default_endpoint: Some(human()),
        queue_bound: 16,
    }
}

fn facade(node: FakeNode) -> FacadeSide<FakeNode, FakeNode> {
    let client = TransportClient::new(
        node.clone(),
        node,
        HumanStore::open_in_memory(StoreOptions::default()).expect("store"),
        ClientConfig {
            client_kind: "human-client".to_owned(),
            endpoint: Some(human()),
            channels: vec![],
            max_payload_bytes: 49_152,
        },
        Box::new(|| 1_786_600_000_000),
        0,
    )
    .expect("facade");
    FacadeSide::new(client, Some(human()), || 1_786_600_000_000)
}

struct NoLinks;

impl Opener for NoLinks {
    fn open(&mut self, _destination: &str) {
        panic!("no link is activated in these tests");
    }
}

fn app(node: &FakeNode, daemon: Option<bool>) -> App<SlintSurface, NoLinks> {
    i_slint_backend_testing::init_no_event_loop();
    let view = View::new().expect("a window");
    let node = node.clone();
    let thread = FacadeThread::spawn(move || Ok(facade(node)), move || daemon, Arc::new(|| {}))
        .expect("the facade thread");
    App::new(SlintSurface(view), NoLinks, thread)
}

/// Pump until `done`, or fail after a while.
fn pump_until(
    app: &mut App<SlintSurface, NoLinks>,
    what: &str,
    done: impl Fn(&App<SlintSurface, NoLinks>) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done(app) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        app.pump();
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_message_reaches_the_window_and_closing_releases_the_lease_not_the_daemon() {
    let (a, b) = FakeNetwork::pair(node(), node());
    let mut alice = app(&a, Some(true));
    pump_until(&mut alice, "alice's session", |_| a.open_sessions() == 1);

    // Bob sends from the other node, through a facade side of his own.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let mut bob = facade(b.clone());
    let sent = runtime.block_on(async {
        let _ = bob.turn(0).await;
        bob.execute(
            Command::Send {
                key: ConversationKey::Direct {
                    peer: a.peer().clone(),
                    endpoint: None,
                },
                draft: "hello alice".to_owned(),
            },
            1,
        )
        .await
    });
    assert!(
        sent.iter().any(|u| matches!(u, Update::Sent { .. })),
        "{sent:?}"
    );

    pump_until(&mut alice, "the message in alice's window", |app| {
        let model = app.side().model();
        model
            .conversations()
            .iter()
            .flat_map(|c| model.messages(&c.key))
            .any(|m| m.source == "hello alice")
    });

    assert!(
        alice.close(Duration::from_secs(5)),
        "the facade thread ended"
    );
    assert_eq!(a.open_sessions(), 0, "the lease is released");
    assert!(
        a.shutdown_requests().is_empty(),
        "the daemon was never asked to stop"
    );
}

#[test]
fn with_no_daemon_the_window_says_so_in_place_of_reconnecting() {
    let (a, _b) = FakeNetwork::pair(node(), node());
    // Nothing serves the profile: the node refuses every open.
    a.stop();
    let mut alice = app(&a, Some(false));
    pump_until(&mut alice, "the no-daemon notice", |app| {
        app.side().model().session_notice() == Some(SessionNotice::NoDaemon)
    });
    assert!(alice.close(Duration::from_secs(5)));
}

/// The real IPC binding, no daemon: the facade over the profile's actual
/// sockets reconnects, and the profile lock -- not held -- has the window
/// say no transport runs for the profile, in place of "reconnecting".
#[test]
fn over_the_real_binding_with_no_daemon_the_window_says_so() {
    use interweave_human_desktop::daemon;
    use interweave_human_desktop::ipc::facade_over_ipc;
    use interweave_human_desktop::startup::Profile;
    use interweave_profile_config::{ProfilePaths, XdgRoots};

    let dir = tempfile::tempdir().expect("tempdir");
    let roots = XdgRoots {
        config_home: dir.path().join("config"),
        data_home: dir.path().join("data"),
        state_home: dir.path().join("state"),
        cache_home: dir.path().join("cache"),
        runtime_dir: Some(dir.path().join("run")),
    };
    let paths = ProfilePaths::resolve("desk", &roots).expect("paths");
    let profile = Profile {
        endpoint: human(),
        channels: vec![],
        data_socket: paths.data_socket().expect("data socket"),
        admin_socket: paths.admin_socket().expect("admin socket"),
        paths: paths.clone(),
    };
    let store = HumanStore::open(&profile.store_path(), StoreOptions::default()).expect("store");

    i_slint_backend_testing::init_no_event_loop();
    let view = View::new().expect("a window");
    let thread = FacadeThread::spawn(
        facade_over_ipc(&profile, store),
        move || daemon::present(&paths),
        Arc::new(|| {}),
    )
    .expect("the facade thread");
    let mut alice = App::new(SlintSurface(view), NoLinks, thread);
    pump_until(&mut alice, "the no-daemon notice over IPC", |app| {
        app.side().model().session_notice() == Some(SessionNotice::NoDaemon)
    });
    assert!(alice.close(Duration::from_secs(5)));
}
