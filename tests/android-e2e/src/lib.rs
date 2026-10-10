// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The orchestration `tests/android-e2e` runs between an Android peer and
//! a desktop one (plan §20 gate (c)), behind a seam the device swaps in at:
//! [`Device`]. Until a target build and a device exist, the Android side
//! is [`HostStandIn`] -- the embedded runtime the app's foreground service
//! hosts (`interweave-transport-embedded`, plan §20 step 1), started on
//! this host under an app data directory of its own. What the stand-in
//! does NOT prove is everything the device adds: the Android target
//! build, the platform's process lifecycle, its network callbacks and
//! SELinux-confined app data. A case run against it is evidence about the
//! embedded composition, never about a phone.
//!
//! TEST-ONLY: nothing outside `tests/` depends on this package.

// A harness that cannot set its case up has nothing to report but the
// cause: each `expect` is that case's failure, by name.
#![allow(
    clippy::expect_used,
    reason = "a harness failure is the case's failure"
)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use interweave_local_client_api::{AdminBinding, DataSessionBinding};
use interweave_profile_config::{ProfilePaths, TrustBoundary, create_private_dir_within};
use interweave_profile_identity::{ProfileIdentity, RecoveryPhrase};
use interweave_test_support::e2e;
use interweave_transport_api::TransportIdentity;
use interweave_transport_composition::InProcessBinding;
use interweave_transport_embedded::{EmbeddedHost, EmbeddedLaunch};

pub use interweave_test_support::e2e::{
    PATIENCE, free_port, human, lease_request, relay::Relay, relayed_example_of,
};

/// How long a stand-in's stop lets exchanges in flight settle.
const GRACE: Duration = Duration::from_secs(1);

/// The Android side of a case, as the orchestration drives it. The host
/// stand-in implements it now; the adb-driven device is to implement the
/// same trait, so a case generic over it (`paths.rs`) needs no change to
/// run on either.
pub trait Device {
    /// The data and admin binding the app's clients open their sessions
    /// on: what `interweave-human-transport-client`'s `TransportClient`
    /// is built on, as on the desktop.
    type Binding: DataSessionBinding + AdminBinding + Clone + Send + Sync + 'static;

    /// The profile's `PeerId`, known before the first start; it survives
    /// [`restart`](Self::restart).
    fn peer(&self) -> TransportIdentity;

    /// Provision `config` as the app does before its first start, and
    /// start the runtime on it.
    fn start(&mut self, config: &str);

    /// Stop the runtime with its grace, as the platform's stop does.
    fn stop(&mut self);

    /// Where the runtime listens, for a peer that must be told.
    fn listening(&self) -> Vec<String>;

    /// The binding of the runtime now serving.
    ///
    /// # Panics
    /// While the runtime is down ([`kill`](Self::kill) without a
    /// [`restart`](Self::restart)).
    fn binding(&self) -> Self::Binding;

    /// Process death: the runtime goes with no grace given.
    fn kill(&mut self);

    /// Start again as the same profile, identity and app data directory.
    fn restart(&mut self);
}

/// The profile the stand-in runs, as the app names it.
pub const PROFILE: &str = "human-android";

/// The embedded runtime under an app data directory of its own, standing
/// in for the device.
pub struct HostStandIn {
    // Held for its lifetime: the app data directory lives under it.
    _scratch: tempfile::TempDir,
    app_data_dir: PathBuf,
    peer: TransportIdentity,
    // The identity is not `Clone`, and each start consumes one: a restart
    // rebuilds it from its phrase, so the PeerId is the same by
    // construction, as a device's Keystore-unwrapped key is.
    phrase: RecoveryPhrase,
    host: Option<EmbeddedHost>,
}

impl HostStandIn {
    /// A fresh app data directory and identity, nothing started: the
    /// `PeerId` is known before either side's profile is written, since
    /// each names the other.
    ///
    /// The directory sits under a scratch root made `0700`, as Android's
    /// `/data/data/<package>` is the app's alone; the embedded runtime
    /// root and its private directories are created under it by the host
    /// itself (ADR-0028), so what a device run checks of that layout is
    /// the same check here.
    ///
    /// # Panics
    /// If the directory cannot be made.
    #[must_use]
    pub fn new() -> Self {
        let scratch = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .expect("a scratch root");
        let app_data_dir = scratch.path().join("app");
        std::fs::create_dir(&app_data_dir).expect("the app data directory");
        std::fs::set_permissions(&app_data_dir, std::fs::Permissions::from_mode(0o700))
            .expect("owner-only");
        let identity = ProfileIdentity::generate();
        Self {
            _scratch: scratch,
            app_data_dir,
            peer: identity
                .transport_identity()
                .expect("a generated identity is valid"),
            phrase: identity
                .recovery_phrase()
                .expect("a generated identity has a phrase"),
            host: None,
        }
    }

    /// The app data directory: the trust boundary the platform supplies.
    #[must_use]
    pub fn app_data_dir(&self) -> &Path {
        &self.app_data_dir
    }

    /// The host now serving, for what the seam does not carry.
    ///
    /// # Panics
    /// While the runtime is down.
    #[must_use]
    pub fn host(&self) -> &EmbeddedHost {
        self.host.as_ref().expect("the runtime is up")
    }
}

impl Device for HostStandIn {
    type Binding = InProcessBinding;

    fn peer(&self) -> TransportIdentity {
        self.peer.clone()
    }

    fn start(&mut self, config: &str) {
        provision(&self.app_data_dir, config);
        self.restart();
    }

    fn stop(&mut self) {
        if let Some(host) = self.host.take() {
            off_runtime(|| host.stop(GRACE)).expect("stops");
        }
    }

    fn listening(&self) -> Vec<String> {
        self.host().listening()
    }

    fn binding(&self) -> InProcessBinding {
        self.host().binding()
    }

    /// The host is dropped without `stop`: its tasks are cancelled and
    /// its lock released, as the kernel releases a dead process's.
    /// Destructors still run here, which a killed process's do not --
    /// the stand-in's limit for process death.
    fn kill(&mut self) {
        drop(self.host.take());
    }

    fn restart(&mut self) {
        assert!(self.host.is_none(), "restart of a runtime still up");
        let identity = ProfileIdentity::from_phrase(&self.phrase).expect("the same identity");
        let launch = EmbeddedLaunch {
            app_data_dir: self.app_data_dir.clone(),
            profile: PROFILE.to_owned(),
            identity,
        };
        self.host = Some(off_runtime(|| EmbeddedHost::start(launch)).expect("the stand-in starts"));
    }
}

impl Default for HostStandIn {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for HostStandIn {
    fn drop(&mut self) {
        self.kill();
    }
}

/// `f` on a thread of its own: the host's start and stop block on its own
/// executor, which may not be done on a thread that drives a runtime --
/// and a case's async test is one. The Service calls them from its own
/// thread, so this is that thread.
fn off_runtime<T: Send>(f: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|scope| scope.spawn(f).join().expect("the host thread"))
}

/// `config.yaml` under the host's configuration root, before the first
/// start, as the app provisions it.
fn provision(app_data_dir: &Path, config: &str) {
    let paths = ProfilePaths::resolve_embedded(
        PROFILE,
        TrustBoundary::new(app_data_dir).expect("a boundary"),
    )
    .expect("paths");
    create_private_dir_within(paths.config_dir(), paths.boundary()).expect("config dir");
    std::fs::write(paths.config_file(), config).expect("write");
}

/// The desktop side: a `transport-daemon` in an XDG home of its own,
/// driven over its sockets as the desktop client drives it.
pub struct Desktop {
    /// The daemon's home: its XDG tree and the profile's paths.
    pub home: e2e::Home,
    /// The profile's `PeerId`, its key written before the first start.
    pub peer: TransportIdentity,
    daemon: Option<e2e::Daemon>,
}

impl Desktop {
    /// A home with the profile's key, nothing started.
    #[must_use]
    pub fn new() -> Self {
        let home = e2e::Home::new("human-desktop");
        let peer = home.write_key();
        Self {
            home,
            peer,
            daemon: None,
        }
    }

    /// Write `config` and start the daemon, until both sockets serve.
    pub async fn start(&mut self, config: &str) {
        self.home.write_config(config);
        let mut daemon = self.home.start(&[]);
        daemon.serving(&self.home).await;
        self.daemon = Some(daemon);
    }

    /// The data and admin binding on the daemon's two sockets.
    #[must_use]
    pub fn binding(&self) -> interweave_ipc_client::IpcBinding {
        self.home.binding()
    }

    /// What the daemon logged so far, for a failing case to show.
    #[must_use]
    pub fn log(&self) -> String {
        self.daemon
            .as_ref()
            .map(e2e::Daemon::log)
            .unwrap_or_default()
    }

    /// `SIGTERM` and the daemon's exit, which must be a clean one.
    ///
    /// # Panics
    /// If it exits otherwise, with its log.
    pub async fn stop(&mut self) {
        if let Some(mut daemon) = self.daemon.take() {
            assert!(daemon.terminate().await.success(), "{}", daemon.log());
        }
    }
}

impl Default for Desktop {
    fn default() -> Self {
        Self::new()
    }
}
