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

fn app(node: &FakeNode, daemon: Result<bool, &'static str>) -> App<SlintSurface, NoLinks> {
    i_slint_backend_testing::init_no_event_loop();
    let view = View::new().expect("a window");
    let node = node.clone();
    let thread = FacadeThread::spawn(
        move || Ok(facade(node)),
        move || daemon.map_err(str::to_owned),
        Arc::new(|| {}),
        |_| {},
    )
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
    let mut alice = app(&a, Ok(true));
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
    let mut alice = app(&a, Ok(false));
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
        |_| {},
    )
    .expect("the facade thread");
    let mut alice = App::new(SlintSurface(view), NoLinks, thread);
    pump_until(&mut alice, "the no-daemon notice over IPC", |app| {
        app.side().model().session_notice() == Some(SessionNotice::NoDaemon)
    });
    assert!(alice.close(Duration::from_secs(5)));
}

/// When the profile lock cannot say whether a daemon runs, that is never
/// read as "no daemon": the window keeps saying "reconnecting".
#[test]
fn a_lock_that_cannot_answer_is_never_read_as_no_daemon() {
    static REPORTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let (a, _b) = FakeNetwork::pair(node(), node());
    a.stop();
    i_slint_backend_testing::init_no_event_loop();
    let view = View::new().expect("a window");
    let node_for = a.clone();
    let thread = FacadeThread::spawn(
        move || Ok(facade(node_for)),
        || Err("the state directory is not private".to_owned()),
        Arc::new(|| {}),
        |_| {
            REPORTS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        },
    )
    .expect("the facade thread");
    let mut alice = App::new(SlintSurface(view), NoLinks, thread);
    pump_until(&mut alice, "the reconnecting notice", |app| {
        app.side().model().session_notice() == Some(SessionNotice::Reconnecting)
    });
    // Several probes later, still not "no daemon".
    let until = Instant::now() + Duration::from_millis(2_500);
    while Instant::now() < until {
        alice.pump();
        assert_ne!(
            alice.side().model().session_notice(),
            Some(SessionNotice::NoDaemon)
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        REPORTS.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the same reason is reported once, not every probe"
    );
    assert!(alice.close(Duration::from_secs(5)));
}

/// A facade that could not be built ends its thread at once; closing then
/// returns at once, not after its whole wait.
#[test]
fn closing_a_facade_thread_that_already_ended_returns_at_once() {
    i_slint_backend_testing::init_no_event_loop();
    let view = View::new().expect("a window");
    let thread = FacadeThread::spawn(
        || -> Result<FacadeSide<FakeNode, FakeNode>, _> {
            Err(interweave_human_store::StoreError::AlreadyRead)
        },
        || Ok(true),
        Arc::new(|| {}),
        |_| {},
    )
    .expect("the facade thread");
    let mut alice = App::new(SlintSurface(view), NoLinks, thread);
    std::thread::sleep(Duration::from_millis(200));
    alice.pump();
    let started = Instant::now();
    assert!(alice.close(Duration::from_secs(5)), "it had ended");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "returned at once, not at the deadline: {:?}",
        started.elapsed()
    );
}

/// The opener runs the handler with the link as its one argument, no
/// shell, so shell syntax in a link is inert; and a failure is reported
/// without the link.
#[test]
fn a_link_is_one_argument_to_the_handler_and_never_in_a_report() {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Mutex;

    use interweave_human_desktop::app::DesktopOpener;

    static REPORTS: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let dir = tempfile::tempdir().expect("tempdir");
    let record = dir.path().join("args");
    let marker = dir.path().join("pwned");
    let handler = dir.path().join("handler");
    std::fs::write(
        &handler,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$#\" \"$1\" > '{}'\n",
            record.display()
        ),
    )
    .expect("handler");
    std::fs::set_permissions(&handler, std::fs::Permissions::from_mode(0o700)).expect("mode");

    let link = format!(
        "https://example.org/a b;touch {} $(touch {})",
        marker.display(),
        marker.display()
    );
    let mut opener = DesktopOpener::with(handler, |line| {
        REPORTS.lock().expect("reports").push(line.to_owned());
    });
    opener.open(&link);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !record.exists() {
        assert!(Instant::now() < deadline, "the handler ran");
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(100));
    let got = std::fs::read_to_string(&record).expect("record");
    assert_eq!(
        got,
        format!("1\n{link}\n"),
        "one argument, the link verbatim"
    );
    assert!(!marker.exists(), "nothing in the link was run");

    let mut failing = DesktopOpener::with(dir.path().join("no-such-handler"), |line| {
        REPORTS.lock().expect("reports").push(line.to_owned());
    });
    failing.open(&link);
    let reports = REPORTS.lock().expect("reports").clone();
    assert_eq!(reports.len(), 1, "{reports:?}");
    assert!(!reports[0].contains("example.org"), "{reports:?}");
}

/// An opened link's process is reaped: once the handler exits, no
/// defunct process is left for the window's lifetime.
#[test]
#[cfg(target_os = "linux")]
fn an_opened_links_process_is_reaped() {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU32, Ordering};

    use interweave_human_desktop::app::DesktopOpener;

    static PID: AtomicU32 = AtomicU32::new(0);
    let dir = tempfile::tempdir().expect("tempdir");
    let handler = dir.path().join("handler");
    std::fs::write(&handler, "#!/bin/sh\nexit 0\n").expect("handler");
    std::fs::set_permissions(&handler, std::fs::Permissions::from_mode(0o700)).expect("mode");
    let mut opener =
        DesktopOpener::with(handler, |_| {}).on_spawn(|pid| PID.store(pid, Ordering::SeqCst));
    opener.open("https://example.org/");
    let pid = PID.load(Ordering::SeqCst);
    assert_ne!(pid, 0, "the handler started");
    // Reaped means gone from the process table, not left as a zombie.
    let proc = std::path::PathBuf::from(format!("/proc/{pid}"));
    assert!(
        std::path::Path::new("/proc/self").exists(),
        "the check reads /proc"
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while proc.exists() {
        assert!(Instant::now() < deadline, "process {pid} was left behind");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// After "no daemon", a lock that can no longer answer clears that
/// guidance: the window falls back to "reconnecting" rather than keep
/// telling the person to start a daemon that may be running, or may not
/// be able to start.
#[test]
fn no_daemon_does_not_outlive_a_lock_that_can_no_longer_answer() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    static PROBES: AtomicUsize = AtomicUsize::new(0);
    let (a, _b) = FakeNetwork::pair(node(), node());
    a.stop();
    i_slint_backend_testing::init_no_event_loop();
    let view = View::new().expect("a window");
    let node_for = a.clone();
    let thread = FacadeThread::spawn(
        move || Ok(facade(node_for)),
        || {
            if PROBES.fetch_add(1, Ordering::SeqCst) == 0 {
                Ok(false)
            } else {
                Err("the state directory is not private".to_owned())
            }
        },
        Arc::new(|| {}),
        |_| {},
    )
    .expect("the facade thread");
    let mut alice = App::new(SlintSurface(view), NoLinks, thread);
    pump_until(&mut alice, "the no-daemon notice", |app| {
        app.side().model().session_notice() == Some(SessionNotice::NoDaemon)
    });
    pump_until(&mut alice, "the notice to fall back", |app| {
        app.side().model().session_notice() == Some(SessionNotice::Reconnecting)
    });
    assert!(alice.close(Duration::from_secs(5)));
}

/// A window that takes nothing holds the facade to its bound: the facade
/// thread waits once `capacity` messages are untaken, so what bob can get
/// accepted is at most that, one turn's drain and alice's endpoint queue
/// -- not everything he sends -- and the window is asked for one wake,
/// not one per message. When the window takes again, every message
/// arrives, once.
#[test]
fn a_window_that_stops_taking_holds_the_facade_to_its_bound() {
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use interweave_human_desktop::facade_thread::FromFacade;
    use interweave_human_transport_client::{ClientEvent, OutboundStatus};

    const CAPACITY: usize = 4;
    const SENT: usize = 200;
    // One turn's drain (app-core's DRAIN_PER_TURN) and the fake
    // endpoint's queue: what a blocked facade can hold beyond the channel.
    const DRAIN: usize = 64;
    let queue = node().queue_bound;

    let (a, b) = FakeNetwork::pair(node(), node());
    let wakes = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&wakes);
    let node_a = a.clone();
    let alice = FacadeThread::spawn_bounded(
        move || Ok(facade(node_a)),
        || Ok(true),
        Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }),
        |_| {},
        CAPACITY,
    )
    .expect("the facade thread");
    let deadline = Instant::now() + Duration::from_secs(10);
    while a.open_sessions() == 0 {
        assert!(Instant::now() < deadline, "alice's session");
        std::thread::sleep(Duration::from_millis(20));
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let mut bob = facade(b.clone());
    let to_alice = ConversationKey::Direct {
        peer: a.peer().clone(),
        endpoint: None,
    };
    let accepted = |updates: &[Update], into: &mut BTreeSet<String>| {
        for update in updates {
            if let Update::Client(ClientEvent::Outbound(o)) = update
                && matches!(o.status, OutboundStatus::Accepted { .. })
            {
                into.insert(o.app_message_id.as_str().to_owned());
            }
        }
    };
    let mut done = BTreeSet::new();
    runtime.block_on(async {
        let _ = bob.turn(0).await;
        for n in 0..SENT {
            let updates = bob
                .execute(
                    Command::Send {
                        key: to_alice.clone(),
                        draft: format!("message {n}"),
                    },
                    1,
                )
                .await;
            accepted(&updates, &mut done);
        }
        // Bob keeps retrying for a while; alice's window takes nothing.
        let until = Instant::now() + Duration::from_secs(3);
        let mut now = 2;
        while Instant::now() < until {
            let updates = bob.turn(now).await;
            accepted(&updates, &mut done);
            now += 1_000;
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });
    assert!(
        done.len() <= CAPACITY + DRAIN + queue,
        "accepted {} of {SENT} with the window stalled: the facade outran its bound",
        done.len()
    );
    assert!(done.len() < SENT, "control: the stall held some back");
    assert_eq!(
        wakes.load(Ordering::SeqCst),
        1,
        "one wake asked while the window took nothing"
    );

    // The window takes again; bob keeps retrying until all are through.
    let mut received = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut now = 10_000_000;
    while received.len() < SENT {
        assert!(
            Instant::now() < deadline,
            "{} of {SENT} reached the window",
            received.len()
        );
        for message in alice.take() {
            if let FromFacade::Update(update) = message
                && let Update::Received(r) = *update
            {
                received.push(r.envelope.text);
            }
        }
        runtime.block_on(async {
            let updates = bob.turn(now).await;
            accepted(&updates, &mut done);
        });
        now += 1_000;
        std::thread::sleep(Duration::from_millis(10));
    }
    let distinct: BTreeSet<_> = received.iter().collect();
    assert_eq!(distinct.len(), SENT, "every message once");
    assert_eq!(received.len(), SENT, "and none twice");
    assert!(
        alice.close(Duration::from_secs(5)),
        "the facade thread ended"
    );
}
