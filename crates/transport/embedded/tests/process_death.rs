// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Process death and restart, the runtime's half (plan §20 Platform
//! tests; the shell's half -- the Service coming back -- is
//! rust-ui-dev's): a host in another process takes the profile, trusts a
//! peer and leases its endpoint, and is KILLED, never stopped. A fresh
//! host then starts on the same app directory -- the kernel released the
//! dead holder's lock -- serves, still trusts the peer (the overlay was
//! written before the trust change answered), and leases the endpoint
//! the dead process held: that lease died with its process. The control:
//! while the child lives, the start is refused `LockHeld`.

#![allow(clippy::expect_used, clippy::panic)]
#![cfg(target_os = "linux")]

use std::collections::BTreeSet;
use std::io::{BufRead as _, BufReader};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use interweave_local_client_api::{
    AdminBinding as _, AdminCapability, AdminPort as _, DataCapability, DataSessionBinding as _,
    SessionRequest,
};
use interweave_profile_config::provision::provision_embedded;
use interweave_profile_config::{ProfileConfig, ProfilePaths, TrustBoundary};
use interweave_profile_identity::{ENTROPY_BYTES, ProfileIdentity, RecoveryPhrase};
use interweave_transport_embedded::{EmbeddedHost, EmbeddedLaunch, EmbeddedRefused};

const PROFILE: &str = "work";
const CHILD_APP: &str = "INTERWEAVE_TEST_PROCESS_DEATH_APP";

/// The peer the child trusts, the same in both processes: a TEST-ONLY
/// identity from fixed entropy, never a real key.
fn trusted_peer() -> ProfileIdentity {
    let phrase = RecoveryPhrase::from_entropy(&[0x5a; ENTROPY_BYTES]).expect("test entropy");
    ProfileIdentity::from_phrase(&phrase).expect("a test identity")
}

fn launch(app: &Path) -> EmbeddedLaunch {
    EmbeddedLaunch {
        app_data_dir: app.to_path_buf(),
        profile: PROFILE.to_owned(),
        identity: ProfileIdentity::generate(),
    }
}

/// The provisioned profile's own endpoint, read back from it.
fn lease(host: &EmbeddedHost) -> Result<(), String> {
    let endpoint = ProfileConfig::load(host.paths())
        .expect("the provisioned profile")
        .endpoints
        .entries
        .into_iter()
        .next()
        .expect("one endpoint")
        .id;
    let request = SessionRequest::new(
        "process-death",
        Some(endpoint),
        [DataCapability::Commands, DataCapability::Events],
    )
    .expect("in bounds");
    host.runtime()
        .block_on(host.binding().open(request))
        .map(drop)
        .map_err(|e| format!("{e:?}"))
}

fn admin_trust(host: &EmbeddedHost) -> impl interweave_local_client_api::AdminPort {
    let capabilities: BTreeSet<AdminCapability> =
        [AdminCapability::Trust, AdminCapability::Status].into();
    host.runtime()
        .block_on(host.binding().admin(capabilities))
        .expect("an admin port")
}

/// The child's side: host the profile, trust the peer, lease the
/// endpoint, say so, and wait to be killed -- or, should the parent die
/// first, for its stdin to close, so no child outlives the run.
#[test]
fn child_hosts_trusts_and_leases_when_asked() {
    let Some(app) = std::env::var_os(CHILD_APP) else {
        return;
    };
    let peer = trusted_peer().transport_identity().expect("a peer");
    let host = EmbeddedHost::start(launch(Path::new(&app))).expect("the child hosts");
    let port = admin_trust(&host);
    host.runtime()
        .block_on(port.set_trust(peer, true))
        .expect("trusted and persisted");
    lease(&host).expect("the child leases the endpoint");
    println!("SERVING");
    let mut line = String::new();
    while std::io::stdin()
        .read_line(&mut line)
        .is_ok_and(|read| read > 0)
    {}
}

/// The child, killed and reaped however the test ends: a panic before
/// the kill must not leave a host holding a profile whose directory the
/// unwinding test is about to delete. Its stdin closes with it.
struct Supervised(Child);

impl Drop for Supervised {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn app() -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .expect("tempdir");
    let app = root.path().join("app");
    std::fs::create_dir(&app).expect("mkdir");
    std::fs::set_permissions(&app, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    (root, app)
}

#[test]
fn a_killed_host_leaves_a_profile_a_fresh_host_starts_on() {
    let (_root, app) = app();
    let paths =
        ProfilePaths::resolve_embedded(PROFILE, TrustBoundary::new(&app).expect("a boundary"))
            .expect("paths");
    provision_embedded(&paths).expect("provisioned");
    let peer = trusted_peer().transport_identity().expect("a peer");

    let mut child = Supervised(
        Command::new(std::env::current_exe().expect("this test binary"))
            .args([
                "child_hosts_trusts_and_leases_when_asked",
                "--exact",
                "--nocapture",
                "--test-threads",
                "1",
            ])
            .env(CHILD_APP, &app)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the child starts"),
    );
    // Read on a thread, so the deadline bounds a child that hangs before
    // it writes anything, not only one that writes the wrong thing.
    let stdout = child.0.stdout.take().expect("piped");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let line = rx.recv_timeout(left).expect("the child serves within 30 s");
        // `contains`: libtest prints the test's name before the line.
        if line.contains("SERVING") {
            break;
        }
    }

    // THE CONTROL: the living child holds the profile.
    assert!(
        matches!(
            EmbeddedHost::start(launch(&app)),
            Err(EmbeddedRefused::LockHeld(_))
        ),
        "refused while the child lives"
    );

    child.0.kill().expect("killed");
    child.0.wait().expect("reaped");

    let host = EmbeddedHost::start(launch(&app)).expect("a fresh host starts after the kill");
    let port = admin_trust(&host);
    let view = host
        .runtime()
        .block_on(port.trust())
        .expect("the trust view");
    assert!(
        view.allowed.iter().any(|row| row.peer == peer),
        "the trust the dead process set persisted: {view:?}"
    );
    // Leases are held in memory today, so this cannot fail now: it guards
    // a lease that is ever made to persist, which a dead holder must not
    // keep.
    lease(&host).expect("the endpoint the dead process leased is free");
    drop(port);
    host.stop(Duration::from_secs(1)).expect("stops");
}
