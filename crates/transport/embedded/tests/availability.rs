// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The person's Stay-reachable choice through the host (ADR-0041 A
//! 2026-10-10): the one write path, the effective mode the host
//! answers, the waiter released when that mode moves away from the one
//! the host started in -- and not when it does not -- the next host
//! starting in the new mode, and an overlay that cannot be trusted
//! refusing the start.

#![allow(clippy::expect_used, clippy::panic)]
#![cfg(target_os = "linux")]

use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use interweave_profile_config::availability_overlay::path_for;
use interweave_profile_config::provision::{embedded_document, provision_embedded};
use interweave_profile_config::{ProfilePaths, TrustBoundary, persist};
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_embedded::{
    AvailabilityMode, EmbeddedHost, EmbeddedLaunch, EmbeddedRefused, Ended, StayReachable,
};

const PROFILE: &str = "work";

/// How long a waiter that must NOT return is watched before the test
/// releases it another way.
const QUIET: Duration = Duration::from_millis(500);
/// How long a waiter that must return may take.
const PATIENCE: Duration = Duration::from_secs(10);

struct App {
    _root: tempfile::TempDir,
    dir: PathBuf,
    paths: ProfilePaths,
}

/// An owner-only app data directory with a provisioned profile, its
/// authored mode `authored`.
fn provisioned(authored: AvailabilityMode) -> App {
    let root = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .expect("tempdir");
    let dir = root.path().join("app");
    std::fs::create_dir(&dir).expect("mkdir");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let paths =
        ProfilePaths::resolve_embedded(PROFILE, TrustBoundary::new(&dir).expect("a boundary"))
            .expect("paths");
    provision_embedded(&paths).expect("provisioned");
    if authored == AvailabilityMode::StayReachable {
        let document = embedded_document(PROFILE).replacen(
            "  android:\n    endpoint: human\n",
            "  android:\n    endpoint: human\n    availability_mode: stay-reachable\n",
            1,
        );
        assert!(document.contains("availability_mode: stay-reachable"));
        persist::write_private_atomic_within(
            &paths.config_file(),
            document.as_bytes(),
            paths.boundary(),
        )
        .expect("authored");
    }
    App {
        _root: root,
        dir,
        paths,
    }
}

fn start(app: &App) -> Result<EmbeddedHost, EmbeddedRefused> {
    EmbeddedHost::start(EmbeddedLaunch {
        app_data_dir: app.dir.clone(),
        profile: PROFILE.to_owned(),
        identity: ProfileIdentity::generate(),
    })
}

/// The Service's waiter, on a thread of its own.
fn waiter(host: &Arc<EmbeddedHost>) -> (Receiver<Ended>, std::thread::JoinHandle<()>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let waiting = Arc::clone(host);
    let thread = std::thread::spawn(move || {
        let _ = tx.send(waiting.wait_shutdown_requested());
    });
    (rx, thread)
}

/// Release a waiter that should still be waiting with a request, and
/// show that the request -- not a changed mode -- is what released it.
fn released_by_request(host: &EmbeddedHost, rx: &Receiver<Ended>) {
    host.request_shutdown(Duration::from_millis(100))
        .expect("asked");
    let ended = rx.recv_timeout(PATIENCE).expect("the waiter returns");
    assert!(
        matches!(ended, Ended::ShutdownRequested(_)),
        "a request released it, not a changed mode: {ended:?}"
    );
}

fn stop(host: Arc<EmbeddedHost>) {
    Arc::into_inner(host)
        .expect("the only holder")
        .stop(Duration::from_secs(1))
        .expect("stops");
}

#[test]
fn turning_stay_reachable_on_restarts_the_service_in_the_new_mode() {
    let app = provisioned(AvailabilityMode::ForegroundOnly);
    let host = Arc::new(start(&app).expect("starts"));
    assert_eq!(host.availability(), AvailabilityMode::ForegroundOnly);
    let (rx, thread) = waiter(&host);
    // THE CONTROL: nothing has changed, and the waiter waits.
    assert!(rx.recv_timeout(QUIET).is_err(), "nothing released it yet");

    host.set_availability(Some(StayReachable)).expect("kept");
    assert_eq!(host.availability(), AvailabilityMode::StayReachable);
    assert_eq!(
        rx.recv_timeout(PATIENCE).expect("the waiter returns"),
        Ended::AvailabilityChanged(AvailabilityMode::StayReachable)
    );
    thread.join().expect("the waiter ends");
    // Until the Service restarts, every wait answers the change at once.
    assert_eq!(
        host.wait_shutdown_requested(),
        Ended::AvailabilityChanged(AvailabilityMode::StayReachable)
    );
    stop(host);

    // The next host starts in the chosen mode, and waits.
    let host = Arc::new(start(&app).expect("starts again"));
    assert_eq!(host.availability(), AvailabilityMode::StayReachable);
    let (rx, thread) = waiter(&host);
    assert!(rx.recv_timeout(QUIET).is_err(), "its own mode is no change");

    // Off removes the entry and is a change back.
    host.set_availability(None).expect("removed");
    assert!(!path_for(&app.paths).exists(), "off removes the entry");
    assert_eq!(
        rx.recv_timeout(PATIENCE).expect("the waiter returns"),
        Ended::AvailabilityChanged(AvailabilityMode::ForegroundOnly)
    );
    thread.join().expect("the waiter ends");
    stop(host);
}

#[test]
fn a_choice_that_leaves_the_effective_mode_changes_nothing() {
    // Authored stay-reachable: choosing it again moves nothing.
    let app = provisioned(AvailabilityMode::StayReachable);
    let host = Arc::new(start(&app).expect("starts"));
    assert_eq!(host.availability(), AvailabilityMode::StayReachable);
    let (rx, thread) = waiter(&host);
    host.set_availability(Some(StayReachable)).expect("kept");
    assert!(path_for(&app.paths).exists(), "the choice is recorded");
    assert!(
        rx.recv_timeout(QUIET).is_err(),
        "the same mode is no change"
    );
    released_by_request(&host, &rx);
    thread.join().expect("the waiter ends");
    stop(host);

    // And a change undone before the Service looked is no change either:
    // the waiter compares against the mode the host STARTED in.
    let other = provisioned(AvailabilityMode::ForegroundOnly);
    let host = Arc::new(start(&other).expect("starts"));
    host.set_availability(Some(StayReachable)).expect("on");
    host.set_availability(None).expect("off");
    assert_eq!(host.availability(), AvailabilityMode::ForegroundOnly);
    let (rx, thread) = waiter(&host);
    assert!(rx.recv_timeout(QUIET).is_err(), "back where it started");
    released_by_request(&host, &rx);
    thread.join().expect("the waiter ends");
    stop(host);
}

#[test]
fn an_overlay_that_cannot_be_trusted_refuses_the_start() {
    let app = provisioned(AvailabilityMode::ForegroundOnly);
    let path = path_for(&app.paths);
    // `foreground-only` is never written; a file saying it is not the
    // overlay, and is not read as off.
    persist::write_private_atomic_within(
        &path,
        br#"{"availability_mode":"foreground-only"}"#,
        app.paths.boundary(),
    )
    .expect("planted");
    let refused = start(&app).err();
    assert!(
        matches!(refused, Some(EmbeddedRefused::ProfileInvalid(_))),
        "{refused:?}"
    );

    // Readable by others: refused as a directory a person can act on.
    persist::write_private_atomic_within(
        &path,
        br#"{"availability_mode":"stay-reachable"}"#,
        app.paths.boundary(),
    )
    .expect("planted");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    let refused = start(&app).err();
    assert!(
        matches!(refused, Some(EmbeddedRefused::DirectoryRefused(_))),
        "{refused:?}"
    );

    // THE CONTROL: the same file, private, starts the host in its mode.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    let host = start(&app).expect("starts");
    assert_eq!(host.availability(), AvailabilityMode::StayReachable);
    host.stop(Duration::from_secs(1)).expect("stops");
}

/// When more than one end holds, the Service hears them in order: a
/// request before a changed mode, and the runtime's end before a changed
/// mode -- a platform stop must not come back as a restart. Each pair is
/// made to hold before the waits, and each wait is asked many times:
/// without the select's bias the pick between two ready arms is random.
#[test]
fn a_request_and_a_runtime_end_win_over_a_changed_mode() {
    let app = provisioned(AvailabilityMode::ForegroundOnly);
    let host = start(&app).expect("starts");
    host.set_availability(Some(StayReachable)).expect("on");
    assert_eq!(
        host.wait_shutdown_requested(),
        Ended::AvailabilityChanged(AvailabilityMode::StayReachable),
        "alone, the change is what the Service hears"
    );
    host.request_shutdown(Duration::from_millis(100))
        .expect("asked");
    for _ in 0..32 {
        assert!(
            matches!(host.wait_shutdown_requested(), Ended::ShutdownRequested(_)),
            "the request wins over a changed mode"
        );
    }
    host.stop(Duration::from_secs(1)).expect("stops");

    let other = provisioned(AvailabilityMode::ForegroundOnly);
    let host = start(&other).expect("starts");
    host.end_runtime_for_test();
    assert_eq!(
        host.wait_shutdown_requested(),
        Ended::RuntimeEnded,
        "alone, the end is what the Service hears"
    );
    host.set_availability(Some(StayReachable)).expect("on");
    for _ in 0..32 {
        assert_eq!(
            host.wait_shutdown_requested(),
            Ended::RuntimeEnded,
            "the runtime's end wins over a changed mode"
        );
    }
    host.stop(Duration::from_secs(1)).expect("stops");
}

/// A write that fails answers an error and leaves the host answering
/// what is on disk: a failed turn-on leaves the authored mode and
/// releases no waiter, a failed turn-off leaves the choice in place.
/// The state directory made read-only (still owner-only, so it is judged
/// private) refuses the temporary file and the unlink alike.
#[test]
fn a_failed_write_leaves_the_mode_on_disk() {
    let app = provisioned(AvailabilityMode::ForegroundOnly);
    let host = Arc::new(start(&app).expect("starts"));
    let state = app.paths.state_dir().to_path_buf();
    let read_only = |mode: u32| {
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(mode)).expect("chmod");
    };

    read_only(0o500);
    let refused = host.set_availability(Some(StayReachable));
    read_only(0o700);
    assert!(refused.is_err(), "the write was refused: {refused:?}");
    assert!(!path_for(&app.paths).exists(), "nothing was written");
    assert_eq!(host.availability(), AvailabilityMode::ForegroundOnly);
    let (rx, thread) = waiter(&host);
    assert!(rx.recv_timeout(QUIET).is_err(), "no change was published");
    released_by_request(&host, &rx);
    thread.join().expect("the waiter ends");
    stop(host);

    // Started stay-reachable, a refused turn-off keeps the choice.
    let host = start(&app).expect("starts");
    host.set_availability(Some(StayReachable)).expect("on");
    host.stop(Duration::from_secs(1)).expect("stops");
    let host = start(&app).expect("starts again");
    assert_eq!(host.availability(), AvailabilityMode::StayReachable);
    read_only(0o500);
    let refused = host.set_availability(None);
    read_only(0o700);
    assert!(refused.is_err(), "the removal was refused: {refused:?}");
    assert!(path_for(&app.paths).exists(), "the choice is still there");
    assert_eq!(host.availability(), AvailabilityMode::StayReachable);
    host.stop(Duration::from_secs(1)).expect("stops");
}
