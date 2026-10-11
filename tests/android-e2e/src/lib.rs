// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The orchestration `tests/android-e2e` runs between an Android peer and
//! a desktop one (plan §20 gate (c)), behind a seam the device swaps in at:
//! [`Device`]. No method of the seam yields a binding to the Android side
//! (architect-cto's DECISION of 2026-10-10), since on a phone no host
//! process can hold one: it drives the lifecycle and names a case of
//! `interweave-android-e2e-cases` for the Android side to run in its own
//! process, and reads the result back. The stand-in exposes its own
//! binding beside the seam, for the host-only cases (`human_chat.rs`). The desktop's half
//! of each case is here.
//!
//! Two runners implement the seam. [`HostStandIn`] -- the embedded
//! runtime the app's foreground service hosts (`interweave-transport-
//! embedded`, plan §20 step 1), started on this host under an app data
//! directory of its own -- runs the case body on a thread beside the test,
//! and proves the body before a phone runs it. What it does NOT prove is
//! everything the device adds: the Android target build, the platform's
//! process lifecycle, its network callbacks and SELinux-confined app
//! data. A case run against it is evidence about the embedded
//! composition, never about a phone. [`adb::AdbDevice`] runs the same
//! case in the app's instrumentation on a real device.
//!
//! TEST-ONLY: nothing outside `tests/` depends on this package.

// A harness that cannot set its case up has nothing to report but the
// cause: each `expect` is that case's failure, by name.
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "a harness failure is the case's failure"
)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use interweave_android_e2e_cases::{
    CaseCtx, Told, keys, send_until_routed, to_android, to_desktop,
};
use interweave_local_client_api::DataSessionPort;
use serde_json::{Map, Value};

use interweave_profile_config::{ProfilePaths, TrustBoundary, create_private_dir_within};
use interweave_profile_identity::{ProfileIdentity, RecoveryPhrase};
use interweave_test_support::e2e;
use interweave_transport_api::TransportIdentity;
use interweave_transport_composition::InProcessBinding;
use interweave_transport_embedded::{EmbeddedHost, EmbeddedLaunch};

pub use interweave_test_support::e2e::{
    PATIENCE, free_port, human, lease_request, relay::Relay, relayed_example_of, schema_validator,
};

pub mod adb;

/// How long a stand-in's stop lets exchanges in flight settle.
const GRACE: Duration = Duration::from_secs(1);

/// The Android side of a case, as the orchestration drives it: its
/// lifecycle, and a named case run in its own process. The host stand-in
/// and the adb-driven device both implement it, so a case generic over it
/// (`paths.rs`) runs on either; a binding to the Android side is no part
/// of it, since on a phone no host process can hold one.
pub trait Device {
    /// The profile's `PeerId`, known before the first start; it survives
    /// [`restart`](Self::restart).
    fn peer(&self) -> TransportIdentity;

    /// Make this host's loopback `port` reachable from the Android side at
    /// the same address, before a configuration names it. The stand-in
    /// shares this host's loopback already.
    fn reach(&mut self, port: u16) {
        let _ = port;
    }

    /// Provision `config` as the app does before its first start, and
    /// start the runtime on it.
    fn start(&mut self, config: &str);

    /// Stop the runtime with its grace, as the platform's stop does.
    fn stop(&mut self);

    /// Process death: the runtime goes with no grace given.
    fn kill(&mut self);

    /// Start again as the same profile, identity and app data directory.
    fn restart(&mut self);

    /// Run `case` of `interweave-android-e2e-cases` with `args` on the
    /// Android side, in its own process, while the caller plays the
    /// desktop's half.
    fn run_case(&self, case: &str, args: &Value) -> CaseRun;

    /// What the Android side logged, for a failing case to show beside
    /// the desktop's. The stand-in's runtime logs into this test's own
    /// output, so it has nothing more to add.
    fn log(&self) -> String {
        String::new()
    }
}

/// A case running on the Android side; [`passed`](Self::passed) waits
/// for its result.
pub struct CaseRun {
    case: String,
    result: std::thread::JoinHandle<String>,
}

impl CaseRun {
    /// `case`, whose result JSON `result` answers.
    #[must_use]
    pub fn on_thread(case: &str, result: impl FnOnce() -> String + Send + 'static) -> Self {
        Self {
            case: case.to_owned(),
            result: std::thread::spawn(result),
        }
    }

    /// The case's result JSON, whatever it says.
    ///
    /// # Panics
    /// If the runner died, or answered something that is not a result.
    #[must_use]
    pub fn result(self) -> Map<String, Value> {
        let json = self.result.join().expect("the runner");
        match serde_json::from_str(&json) {
            Ok(Value::Object(out)) => out,
            _ => panic!("{}: not a result: {json}", self.case),
        }
    }

    /// The result of a case that held.
    ///
    /// # Panics
    /// If it did not, with its detail and `log`.
    #[must_use]
    pub fn passed(self, log: impl FnOnce() -> String) -> Map<String, Value> {
        let case = self.case.clone();
        let out = self.result();
        assert_eq!(
            out.get(keys::RESULT).and_then(Value::as_str),
            Some(keys::PASS),
            "{case} on the Android side: {:?}\n{}",
            out.get(keys::DETAIL),
            log()
        );
        out
    }
}

/// The desktop's half of exchange `serial` with the Android side `from`:
/// take what it sent, and answer, each within `PATIENCE`.
///
/// # Panics
/// If either does not happen, with `log`.
pub async fn desktop_answers(
    session: &impl DataSessionPort,
    told: &mut Told,
    from: &TransportIdentity,
    serial: u8,
    log: impl Fn() -> String,
) {
    let (_, text) = to_desktop(serial);
    if let Err(e) = told.take_message(session, from, &text, PATIENCE).await {
        panic!("{e}\n{}", log());
    }
    let (id, answer) = to_android(serial);
    if let Err(e) = send_until_routed(session, from, id, &answer, PATIENCE).await {
        panic!("{e}\n{}", log());
    }
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

    /// Where the runtime listens.
    #[must_use]
    pub fn listening(&self) -> Vec<String> {
        self.host().listening()
    }

    /// The binding of the runtime now serving: the stand-in's own, for
    /// what a host-only case drives directly (`human_chat.rs`).
    ///
    /// # Panics
    /// While the runtime is down.
    #[must_use]
    pub fn binding(&self) -> InProcessBinding {
        self.host().binding()
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

    /// The case body on a thread of its own, as the instrumentation runs
    /// it on its own: over the binding of the runtime now serving, if one
    /// is, and with the stand-in's provisioning.
    fn run_case(&self, case: &str, args: &Value) -> CaseRun {
        let ctx = CaseCtx {
            peer: self.peer.clone(),
            binding: self.host.as_ref().map(EmbeddedHost::binding),
            provision: Some(Box::new({
                let app_data_dir = self.app_data_dir.clone();
                move |config: &str| try_provision(&app_data_dir, config)
            })),
            client: None,
        };
        let (name, args) = (case.to_owned(), args.to_string());
        CaseRun::on_thread(case, move || {
            interweave_android_e2e_cases::run(&name, &args, ctx)
        })
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
    try_provision(app_data_dir, config).expect("provisioned");
}

fn try_provision(app_data_dir: &Path, config: &str) -> Result<(), String> {
    let boundary = TrustBoundary::new(app_data_dir).map_err(|e| format!("a boundary: {e}"))?;
    let paths =
        ProfilePaths::resolve_embedded(PROFILE, boundary).map_err(|e| format!("paths: {e}"))?;
    create_private_dir_within(paths.config_dir(), paths.boundary())
        .map_err(|e| format!("the config directory: {e}"))?;
    std::fs::write(paths.config_file(), config).map_err(|e| format!("config.yaml: {e}"))
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
