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
use interweave_transport_api::TransportIdentity;
use interweave_transport_composition::InProcessBinding;
use interweave_transport_embedded::{EmbeddedHost, EmbeddedLaunch};

/// How long a stand-in's stop lets exchanges in flight settle.
const GRACE: Duration = Duration::from_secs(1);

/// The Android side of a case, as the orchestration drives it. The host
/// stand-in implements it now; the adb-driven device implements the same
/// trait, so a case written against it runs unchanged on either.
pub trait Device {
    /// The data and admin binding the app's clients open their sessions
    /// on: what `interweave-human-transport-client`'s `TransportClient`
    /// is built on, as on the desktop.
    type Binding: DataSessionBinding + AdminBinding + Clone + Send + Sync + 'static;

    /// The profile's `PeerId`; it survives [`restart`](Self::restart).
    fn peer(&self) -> TransportIdentity;

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
    /// Provision `config` (an `embedded-android` profile) under a fresh
    /// app data directory and start the runtime on it.
    ///
    /// The directory sits under a scratch root made `0700`, as Android's
    /// `/data/data/<package>` is the app's alone; the embedded runtime
    /// root and its private directories are created under it by the host
    /// itself (ADR-0028), so what a device run checks of that layout is
    /// the same check here.
    ///
    /// # Panics
    /// If provisioning or the start fails: the case cannot run.
    #[must_use]
    pub fn start(config: &str) -> Self {
        let scratch = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .expect("a scratch root");
        let app_data_dir = scratch.path().join("app");
        std::fs::create_dir(&app_data_dir).expect("the app data directory");
        std::fs::set_permissions(&app_data_dir, std::fs::Permissions::from_mode(0o700))
            .expect("owner-only");
        provision(&app_data_dir, config);
        let identity = ProfileIdentity::generate();
        let mut stand_in = Self {
            _scratch: scratch,
            app_data_dir,
            peer: identity
                .transport_identity()
                .expect("a generated identity is valid"),
            phrase: identity
                .recovery_phrase()
                .expect("a generated identity has a phrase"),
            host: None,
        };
        stand_in.restart();
        stand_in
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

    /// Stop the runtime with its grace, as the platform's stop does.
    ///
    /// # Panics
    /// If the runtime's driver failed.
    pub fn stop(&mut self) {
        if let Some(host) = self.host.take() {
            host.stop(GRACE).expect("stops");
        }
    }
}

impl Device for HostStandIn {
    type Binding = InProcessBinding;

    fn peer(&self) -> TransportIdentity {
        self.peer.clone()
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
        self.host = Some(
            EmbeddedHost::start(EmbeddedLaunch {
                app_data_dir: self.app_data_dir.clone(),
                profile: PROFILE.to_owned(),
                identity,
            })
            .expect("the stand-in starts"),
        );
    }
}

impl Drop for HostStandIn {
    fn drop(&mut self) {
        self.kill();
    }
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
