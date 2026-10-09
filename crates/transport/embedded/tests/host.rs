// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The embedded host on the host machine (plan §20 step 1): started under
//! an app data directory whose parent every walk to `/` refuses -- the
//! stand-in for Android's `system`-owned `/data/data` -- serving its
//! binding, keeping its state under its own root, holding the profile
//! against a second start, and refusing each cause by name. Each refusal
//! runs beside the start that succeeds.

#![allow(clippy::expect_used, clippy::panic)]
#![cfg(target_os = "linux")]

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use interweave_local_client_api::{AdminBinding, AdminCapability, AdminPort};
use interweave_profile_config::trust_overlay::TrustOverlay;
use interweave_profile_config::{ProfilePaths, TrustBoundary, create_private_dir_within};
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_embedded::{EmbeddedHost, EmbeddedLaunch, EmbeddedRefused};

const PROFILE: &str = "work";

/// `T/open/app`: `open` other-writable and not sticky, `app` ours and
/// `0700`.
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

fn document(deployment: &str, ipc: bool) -> String {
    format!(
        "schema_version: 2
profile:
  name: {PROFILE}
runtime:
  deployment: {deployment}
ipc:
  enabled: {ipc}
trust:
  policy: static-allowlist
  allowed_peers: []
endpoints:
  entries:
    - id: human
      enabled: true
      advertise: false
transport:
  listen:
    addresses: [\"/ip4/127.0.0.1/tcp/0\"]
"
    )
}

/// Provision `config.yaml` under the host's configuration root, as the
/// app does before its first start.
fn provision(app: &Path, text: &str) -> ProfilePaths {
    let paths =
        ProfilePaths::resolve_embedded(PROFILE, TrustBoundary::new(app).expect("a boundary"))
            .expect("paths");
    create_private_dir_within(paths.config_dir(), paths.boundary()).expect("config dir");
    std::fs::write(paths.config_file(), text).expect("write");
    std::fs::set_permissions(paths.config_file(), std::fs::Permissions::from_mode(0o644))
        .expect("chmod");
    paths
}

fn launch(app: &Path) -> EmbeddedLaunch {
    EmbeddedLaunch {
        app_data_dir: app.to_path_buf(),
        profile: PROFILE.to_owned(),
        identity: ProfileIdentity::generate(),
    }
}

fn admin(host: &EmbeddedHost) -> impl AdminPort {
    let capabilities: BTreeSet<AdminCapability> = [
        AdminCapability::Status,
        AdminCapability::Trust,
        AdminCapability::Shutdown,
    ]
    .into_iter()
    .collect();
    host.runtime()
        .block_on(host.binding().admin(capabilities))
        .expect("an admin port")
}

/// The host starts under the refused ancestor, serves its binding as the
/// launched identity, creates its root owner-only, and keeps a trust
/// change in the overlay under that root; its paths carry the boundary
/// and the root.
#[test]
fn the_host_serves_under_its_root() {
    let app = app();
    provision(&app.dir, &document("embedded-android", false));
    let launched = launch(&app.dir);
    let peer = launched.identity.transport_identity().expect("peer");
    let host = EmbeddedHost::start(launched).expect("starts");
    let port = admin(&host);
    let status = host.runtime().block_on(port.status()).expect("status");
    assert_eq!(status.peer, peer);
    let listening = host.listening();
    assert_eq!(listening.len(), 1, "{listening:?}");
    assert!(
        listening[0].starts_with("/ip4/127.0.0.1/tcp/"),
        "{listening:?}"
    );
    assert!(
        !listening[0].ends_with("/tcp/0"),
        "the bound port: {listening:?}"
    );

    let root = app.dir.join("interweave");
    assert_eq!(
        std::fs::metadata(&root)
            .expect("the root")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(host.paths().boundary().path(), app.dir);
    assert_eq!(host.paths().boundary().runtime_root(), Some(root.as_path()));

    let other = ProfileIdentity::generate()
        .transport_identity()
        .expect("peer");
    host.runtime()
        .block_on(port.set_trust(other.clone(), true))
        .expect("set and persisted");
    let overlay = TrustOverlay::path_for(host.paths());
    assert!(overlay.starts_with(&root), "{}", overlay.display());
    let (_, effective) =
        TrustOverlay::load_within(&overlay, &BTreeSet::new(), host.paths().boundary())
            .expect("the overlay reads back");
    assert!(effective.contains(&other));
    drop(port);
    host.stop(Duration::from_secs(1)).expect("stops");
}

/// A second start in the same process while a host holds the profile is
/// refused `LockHeld`, and the first keeps serving; once the first has
/// stopped, a start succeeds -- the lock went with it.
#[test]
fn a_second_start_is_refused_while_the_first_serves() {
    let app = app();
    provision(&app.dir, &document("embedded-android", false));
    let first = EmbeddedHost::start(launch(&app.dir)).expect("the first starts");
    match EmbeddedHost::start(launch(&app.dir)) {
        Err(EmbeddedRefused::LockHeld(_)) => {}
        Err(other) => panic!("refused as held: {other:?}"),
        Ok(_) => panic!("refused as held, not started"),
    }
    let port = admin(&first);
    first
        .runtime()
        .block_on(port.status())
        .expect("the first still serves");
    drop(port);
    first.stop(Duration::from_secs(1)).expect("stops");
    EmbeddedHost::start(launch(&app.dir))
        .expect("starts once the first has stopped")
        .stop(Duration::from_secs(1))
        .expect("stops");
}

/// An admin port's shutdown reaches the Service as a request carrying
/// the grace asked for: the runtime does not stop itself.
#[test]
fn a_shutdown_request_reaches_the_owner() {
    let app = app();
    provision(&app.dir, &document("embedded-android", false));
    let host = EmbeddedHost::start(launch(&app.dir)).expect("starts");
    let port = admin(&host);
    host.runtime()
        .block_on(port.shutdown(Duration::from_millis(250)))
        .expect("asked");
    let request = host.wait_shutdown_requested().expect("a request");
    assert_eq!(request.grace, Duration::from_millis(250));
    host.runtime()
        .block_on(port.status())
        .expect("still serving until stopped");
    drop(port);
    host.stop(request.grace).expect("stops");
}

/// Each refusal, by its cause: a daemon profile, a missing or invalid
/// document, an app directory that is absent or itself refused -- named
/// as the trust boundary. Nothing is left holding the profile: the
/// control after them starts, under the same refused `open` above.
#[test]
fn each_refusal_names_its_cause() {
    let app = app();
    let refused = |dir: &Path| match EmbeddedHost::start(launch(dir)) {
        Err(e) => e,
        Ok(_) => panic!("refused"),
    };

    assert!(matches!(
        refused(&app.dir),
        EmbeddedRefused::ProfileInvalid(_)
    ));
    provision(&app.dir, &document("daemon-ipc", true));
    assert_eq!(refused(&app.dir), EmbeddedRefused::NotEmbedded);
    provision(&app.dir, &document("embedded-android", true));
    assert!(matches!(
        refused(&app.dir),
        EmbeddedRefused::ProfileInvalid(_)
    ));

    assert!(matches!(
        refused(&app.dir.join("absent")),
        EmbeddedRefused::DirectoryRefused(_)
    ));
    // A valid document, so what refuses is the boundary itself: the
    // configuration directory's walk reaches it and stops there.
    provision(&app.dir, &document("embedded-android", false));
    std::fs::set_permissions(&app.dir, std::fs::Permissions::from_mode(0o757)).expect("chmod");
    match refused(&app.dir) {
        EmbeddedRefused::DirectoryRefused(detail) => {
            assert!(detail.contains("the trust boundary"), "{detail}");
        }
        other => panic!("refused at the boundary: {other:?}"),
    }
    std::fs::set_permissions(&app.dir, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    EmbeddedHost::start(launch(&app.dir))
        .expect("the control starts")
        .stop(Duration::from_secs(1))
        .expect("stops");
}
