// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The Service's hold on the client, against the real embedded runtime on
//! the host: what the Service starts, what a view attached to it sees,
//! and what a stop leaves behind.

#![allow(clippy::expect_used, clippy::panic)]
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use interweave_human_android_platform::stand_in;
use interweave_human_android_platform::{
    Ended, ServiceHost, ServiceLaunch, StartRefused, ToView, ViewLink,
};
use interweave_human_app_core::Update;
use interweave_human_client_api::{ClientEvent, SessionState};
use interweave_profile_config::{ProfilePaths, TrustBoundary};

const PROFILE: &str = "human";
const WAIT: Duration = Duration::from_secs(20);
const GRACE: Duration = Duration::from_secs(1);

/// An app data directory as the platform hands it over: private, under a
/// parent that is not (Android's `/data/user/0` is the system's).
struct App {
    _root: tempfile::TempDir,
    dir: PathBuf,
}

fn app() -> App {
    let root = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .expect("tempdir");
    let open = root.path().join("open");
    let dir = open.join("app");
    for (path, mode) in [(&open, 0o777), (&dir, 0o700)] {
        std::fs::create_dir(path).expect("mkdir");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
    }
    App { _root: root, dir }
}

fn launch(app: &App) -> ServiceLaunch {
    ServiceLaunch {
        app_data_dir: app.dir.clone(),
        profile: PROFILE.to_owned(),
        identity: stand_in::identity(),
    }
}

fn started(app: &App) -> ServiceHost {
    stand_in::provision(&app.dir, PROFILE).expect("provisioned");
    let service = ServiceHost::new();
    service.start(launch(app)).expect("started");
    service
}

fn attach(service: &ServiceHost) -> ViewLink {
    service.hub().attach(Arc::new(|| {}))
}

/// Take from `link` until `done` says so, or fail after [`WAIT`]. What
/// arrived after the message that satisfied it is kept in `held` for the
/// next call: a take hands over everything that waits, so the next thing
/// a test waits for may have come in the same take.
fn until(
    link: &mut ViewLink,
    held: &mut Vec<ToView>,
    what: &str,
    mut done: impl FnMut(&ToView) -> bool,
) {
    let deadline = Instant::now() + WAIT;
    loop {
        let mut waiting = std::mem::take(held);
        waiting.extend(link.take());
        if let Some(at) = waiting.iter().position(&mut done) {
            *held = waiting.split_off(at + 1);
            return;
        }
        assert!(Instant::now() < deadline, "no {what} within {WAIT:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn is_listing(message: &ToView) -> bool {
    matches!(message, ToView::Update(u) if matches!(**u, Update::Listed(_)))
}

fn is_ready(message: &ToView) -> bool {
    matches!(
        message,
        ToView::Update(u)
            if matches!(**u, Update::Client(ClientEvent::Session(SessionState::Ready { .. })))
    )
}

#[test]
fn a_started_service_gives_a_view_its_listing_then_a_ready_session() {
    let app = app();
    let service = started(&app);
    assert!(service.is_running());
    let mut link = attach(&service);
    let first = link.take();
    assert!(
        matches!(first.first(), Some(ToView::Running(true))),
        "the view hears the service runs first: {first:?}"
    );
    let mut listed = first.iter().any(is_listing);
    let mut ready = false;
    let deadline = Instant::now() + WAIT;
    while !(listed && ready) && Instant::now() < deadline {
        for message in link.take() {
            if is_listing(&message) {
                listed = true;
            } else if is_ready(&message) {
                assert!(listed, "an update never precedes the view's listing");
                ready = true;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(listed && ready, "listed {listed}, ready {ready}");
    let stopped = service.stop(GRACE).expect("it ran");
    assert!(
        stopped.session_closed,
        "the session closed before the runtime stopped"
    );
    assert!(stopped.runtime.is_ok(), "{:?}", stopped.runtime);
}

#[test]
fn a_start_while_the_client_runs_changes_nothing() {
    let app = app();
    let service = started(&app);
    let mut link = attach(&service);
    let mut held = Vec::new();
    until(&mut link, &mut held, "listing", is_listing);
    service
        .start(launch(&app))
        .expect("a second start succeeds");
    assert!(service.is_running());
    let mut again = held;
    again.extend(link.take());
    assert!(
        !again
            .iter()
            .any(|m| matches!(m, ToView::Running(_)) || is_listing(m)),
        "the facade was not restarted under the view: {again:?}"
    );
    let _ = service.stop(GRACE);
}

#[test]
fn a_stop_releases_the_waiter_tells_the_view_and_frees_the_profile() {
    let app = app();
    let service = Arc::new(started(&app));
    let mut link = attach(&service);
    let mut held = Vec::new();
    until(&mut link, &mut held, "listing", is_listing);

    let waiting = Arc::clone(&service);
    let waiter = std::thread::spawn(move || waiting.wait_ended());
    // The waiter is parked in the runtime before the stop asks.
    std::thread::sleep(Duration::from_millis(200));
    let stopped = service.stop(GRACE).expect("it ran");
    assert!(stopped.runtime.is_ok(), "{:?}", stopped.runtime);
    assert_eq!(
        waiter.join().expect("the waiter returned"),
        Ended::ShutdownRequested { grace: GRACE }
    );
    until(&mut link, &mut held, "the service stopping", |m| {
        matches!(m, ToView::Running(false))
    });
    assert!(!service.is_running());
    assert_eq!(service.wait_ended(), Ended::NotRunning);
    assert!(service.stop(GRACE).is_none(), "nothing left to stop");

    // The profile lock went with the runtime: the same profile starts.
    service.start(launch(&app)).expect("started again");
    until(&mut link, &mut held, "the service running again", |m| {
        matches!(m, ToView::Running(true))
    });
    until(&mut link, &mut held, "a fresh listing", is_listing);
    let _ = service.stop(GRACE);
}

#[test]
fn a_profile_no_endpoint_admits_is_refused_and_leaves_nothing_running() {
    let app = app();
    stand_in::provision(&app.dir, PROFILE).expect("provisioned");
    let paths = ProfilePaths::resolve_embedded(PROFILE, TrustBoundary::new(&app.dir).expect("b"))
        .expect("paths");
    let good = std::fs::read_to_string(paths.config_file()).expect("read");
    let bad = good.replace(
        "allowed_client_kinds: [human-client]",
        "allowed_client_kinds: []",
    );
    assert_ne!(good, bad, "the control: the document names the kind");
    std::fs::write(paths.config_file(), &bad).expect("write");

    let service = ServiceHost::new();
    assert_eq!(
        service.start(launch(&app)),
        Err(StartRefused::NoHumanEndpoint)
    );
    assert!(!service.is_running());
    // The runtime it started was stopped: its lock is free again.
    std::fs::write(paths.config_file(), &good).expect("write");
    service
        .start(launch(&app))
        .expect("starts once the profile admits it");
    let _ = service.stop(GRACE);
}

#[test]
fn the_stand_in_profile_is_written_once_and_never_rewritten() {
    let app = app();
    stand_in::provision(&app.dir, PROFILE).expect("provisioned");
    let paths = ProfilePaths::resolve_embedded(PROFILE, TrustBoundary::new(&app.dir).expect("b"))
        .expect("paths");
    let mode = std::fs::metadata(paths.config_dir())
        .expect("stat")
        .permissions()
        .mode();
    assert_eq!(
        mode & 0o777,
        0o700,
        "the configuration directory is private"
    );
    let changed = format!(
        "{}# a person's change\n",
        std::fs::read_to_string(paths.config_file()).expect("read")
    );
    std::fs::write(paths.config_file(), &changed).expect("write");
    stand_in::provision(&app.dir, PROFILE).expect("again");
    assert_eq!(
        std::fs::read_to_string(paths.config_file()).expect("read"),
        changed
    );
}
