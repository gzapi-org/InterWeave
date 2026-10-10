// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The transport runtime hosted in-process (plan §20 step 1): what the
//! Android app's foreground service starts, binds its clients to, and
//! stops -- the Stage 12 composition and its in-process
//! `LocalDataSession`/`LocalAdminPort` binding, not a second adapter
//! (§15 (3)), under the trust boundary the platform supplies (ADR-0028
//! A 2026-10-08).
//!
//! The Service owns the lifecycle and calls in from its own threads:
//! [`EmbeddedHost::start`], [`EmbeddedHost::wait_shutdown_requested`]
//! and [`EmbeddedHost::stop`] BLOCK, and must be called off any async
//! context -- a tokio runtime refuses to be driven from inside one.
//! What the Service's clients need to run the binding's futures is
//! [`EmbeddedHost::runtime`], so the process runs one executor, not two.
//!
//! ON DISK everything lies under one root the host creates owner-only,
//! `<app data dir>/interweave` ([`ProfilePaths::resolve_embedded`]), and
//! [`EmbeddedHost::paths`] carries that root and the boundary to every
//! caller that opens a private directory -- the human store's opener
//! included -- so a directory outside the root, the platform's `files/`
//! among them, is refused whoever asks.

use std::path::PathBuf;
use std::time::Duration;

pub mod custody;

use interweave_local_client_api::{AdminBinding as _, AdminCapability, AdminPort as _};
pub use interweave_profile_config::availability_overlay::StayReachable;
use interweave_profile_config::availability_overlay::{self, AvailabilityError};
pub use interweave_profile_config::runtime::AvailabilityMode;
use interweave_profile_config::sections::LogLevel;
use interweave_profile_config::trust_overlay::OverlayError;
use interweave_profile_config::{
    LoadError, PersistError, ProfileConfig, ProfileLock, ProfilePaths, TrustBoundary,
    runtime::Deployment,
};
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_composition::Ended as ComposedEnded;
use interweave_transport_composition::{
    AUDIT_TARGET, ComposedRuntime, CompositionError, CompositionOptions, InProcessBinding,
};
pub use interweave_transport_composition::{NetworkView, ShutdownRequest};

/// Why [`EmbeddedHost::wait_shutdown_requested`] returned. The Service
/// answers each by stopping the host, and after
/// [`AvailabilityChanged`](Self::AvailabilityChanged) by starting it
/// again in the mode it carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    /// An admin port, or the owner's own
    /// [`request_shutdown`](EmbeddedHost::request_shutdown), asked for a
    /// stop.
    ShutdownRequested(ShutdownRequest),
    /// The runtime stopped without being asked, its substrate gone.
    RuntimeEnded,
    /// The effective availability mode is no longer the one the host
    /// started in: the person's choice changed it
    /// ([`EmbeddedHost::set_availability`]), and the Service restarts in
    /// this mode rather than waiting for the next start (ADR-0041 A
    /// 2026-10-10).
    AvailabilityChanged(AvailabilityMode),
}

/// What the Service starts a host with.
pub struct EmbeddedLaunch {
    /// The app's data directory as the platform reports it -- on Android
    /// `Context.getDataDir()` -- never a hard-coded path: the trust
    /// boundary, resolved here once.
    pub app_data_dir: PathBuf,
    /// The profile's name; its `config.yaml` must already be in place
    /// under the host's configuration root, provisioned before the first
    /// start.
    pub profile: String,
    /// The profile's identity, supplied rather than read: on a device it
    /// is what [`custody::unlock`] gave back from the Keystore-wrapped
    /// record (plan §20 step 6), so the seed never rests in a file the
    /// host could read.
    pub identity: ProfileIdentity,
}

/// Why a host did not start or stop: one variant per cause a person can
/// act on, closed, so the Service maps each to its own stable message
/// and a new cause is a compile error there. The text is for the log
/// only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbeddedRefused {
    /// Another host holds the profile -- in another process, or in this
    /// one: a second [`EmbeddedHost::start`] while one runs is refused
    /// here, and the first keeps serving.
    LockHeld(String),
    /// The profile's `runtime.deployment` is not `embedded-android`.
    NotEmbedded,
    /// `config.yaml` is missing, unreadable or fails validation.
    ProfileInvalid(String),
    /// The trust boundary, or a private directory under it, is refused
    /// by ADR-0028's rules or lies outside the host's root.
    DirectoryRefused(String),
    /// The supplied identity carries no transport identity.
    IdentityRejected,
    /// The runtime failed to start or to stop.
    Internal(String),
}

impl std::fmt::Display for EmbeddedRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LockHeld(detail) => write!(f, "the profile is held by another host: {detail}"),
            Self::NotEmbedded => f.write_str("the profile's deployment is not embedded-android"),
            Self::ProfileInvalid(detail) => write!(f, "the profile cannot be used: {detail}"),
            Self::DirectoryRefused(detail) => write!(f, "a directory is refused: {detail}"),
            Self::IdentityRejected => f.write_str("the identity has no transport identity"),
            Self::Internal(detail) => write!(f, "the runtime failed: {detail}"),
        }
    }
}

impl std::error::Error for EmbeddedRefused {}

impl From<PersistError> for EmbeddedRefused {
    fn from(e: PersistError) -> Self {
        match e {
            PersistError::ProfileLocked { .. } => Self::LockHeld(e.to_string()),
            PersistError::DirectoryNotPrivate { .. } | PersistError::FileNotPrivate { .. } => {
                Self::DirectoryRefused(e.to_string())
            }
            // A directory absent or closed to this app is a directory a
            // person can act on; a full disk or an I/O error is not
            // (#241 review part 2 F2).
            PersistError::Io(ref io)
                if matches!(
                    io.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                ) =>
            {
                Self::DirectoryRefused(e.to_string())
            }
            PersistError::InvalidProfileName { .. } => Self::ProfileInvalid(e.to_string()),
            _ => Self::Internal(e.to_string()),
        }
    }
}

impl From<LoadError> for EmbeddedRefused {
    fn from(e: LoadError) -> Self {
        match e {
            LoadError::ConfigDirUnguarded(_) | LoadError::ConfigFileUnguarded { .. } => {
                Self::DirectoryRefused(e.to_string())
            }
            _ => Self::ProfileInvalid(e.to_string()),
        }
    }
}

impl From<AvailabilityError> for EmbeddedRefused {
    fn from(e: AvailabilityError) -> Self {
        match e {
            AvailabilityError::NotPrivate { .. } => Self::DirectoryRefused(e.to_string()),
            AvailabilityError::Read(io) => Self::from(PersistError::Io(io)),
            AvailabilityError::Write(persist) => Self::from(persist),
            AvailabilityError::Config(load) => Self::from(load),
            AvailabilityError::TooLarge | AvailabilityError::Parse(_) => {
                Self::ProfileInvalid(e.to_string())
            }
        }
    }
}

impl From<CompositionError> for EmbeddedRefused {
    fn from(e: CompositionError) -> Self {
        match e {
            CompositionError::InvalidProfile(_) | CompositionError::Unhonoured { .. } => {
                Self::ProfileInvalid(e.to_string())
            }
            CompositionError::TrustOverlay(OverlayError::NotPrivate { .. }) => {
                Self::DirectoryRefused(e.to_string())
            }
            _ => Self::Internal(e.to_string()),
        }
    }
}

/// The prefix every first-party target begins with: the crates' module
/// paths (`interweave_*`) and the named targets (`interweave::audit`,
/// `interweave::connectivity`).
const FIRST_PARTY: &str = "interweave";

/// Whether the embedded host's log sink admits a record of `target` at
/// `level` under the profile's `observability.log_level`: what the
/// daemon's log admits, so the platform's writer -- logcat, the app's to
/// choose -- carries the same lines (plan §20, carried from §18).
///
/// THE AUDIT TARGET ([`AUDIT_TARGET`]) at INFO WHATEVER THE LEVEL: each
/// trust change's record is the contract's, not a diagnostic
/// (`LOCAL-CLIENT.md` §5, A 2026-10-04). First-party targets at the
/// profile's level; every other crate's at it, capped at WARN.
/// `the_filter_admits_what_the_daemons_does` holds it to a copy of the
/// daemon's filter construction, and
/// `the_copy_is_the_daemons_construction` finds each of the daemon's
/// filter lines in the daemon's source and its counterpart in the copy,
/// so a change to either side of one of those lines fails there.
#[must_use]
pub fn log_admits(target: &str, level: tracing::Level, profile: LogLevel) -> bool {
    let configured = match profile {
        LogLevel::Error => tracing::Level::ERROR,
        LogLevel::Warn => tracing::Level::WARN,
        LogLevel::Info => tracing::Level::INFO,
        LogLevel::Debug => tracing::Level::DEBUG,
    };
    // `tracing` orders the more verbose level as the greater.
    let ceiling = if target.starts_with(AUDIT_TARGET) {
        tracing::Level::INFO
    } else if target.starts_with(FIRST_PARTY) {
        configured
    } else {
        std::cmp::min(configured, tracing::Level::WARN)
    };
    level <= ceiling
}

/// How long [`EmbeddedHost::start`] waits for the profile lock: not at
/// all. Two hosts of one profile in one process is the Service starting
/// twice, which is refused at once; a host that died with its process
/// left no holder, since the kernel releases the lock with it.
const LOCK_WAIT: Duration = Duration::ZERO;

/// The executor's worker threads: a fixed two rather than the daemon's
/// one per CPU, so a phone's runtime keeps a bounded thread count beside
/// the app's UI -- one for the substrate's driver and one for the
/// sessions' work. A choice, not a measurement: no device run has
/// shown whether two is too few or more than needed.
const WORKERS: usize = 2;

/// A running embedded transport runtime and what it holds.
pub struct EmbeddedHost {
    // DROPPED IN THIS ORDER (`Drop` below, `stop`): the runtime's tasks
    // first, then the executor, then the lock -- a lock released while
    // the runtime still writes its state would let a second host write
    // beside it.
    composed: Option<ComposedRuntime>,
    executor: Option<tokio::runtime::Runtime>,
    handle: tokio::runtime::Handle,
    sessions: InProcessBinding,
    paths: ProfilePaths,
    log_level: LogLevel,
    // THE AVAILABILITY CHOICE. `authored` is `config.yaml`'s field,
    // `started_in` the effective mode at start, and `availability` the
    // effective mode now, published so a waiter sees it move; writes are
    // serialised by `availability_writes`, so the file and the published
    // mode change together.
    authored: AvailabilityMode,
    started_in: AvailabilityMode,
    availability: tokio::sync::watch::Sender<AvailabilityMode>,
    availability_writes: std::sync::Mutex<()>,
    lock: Option<ProfileLock>,
}

impl EmbeddedHost {
    /// Resolve the profile under `launch.app_data_dir`, load and check
    /// it, take the profile lock, and start the composed runtime on an
    /// executor of the host's own.
    ///
    /// BLOCKS; call it off any async context.
    ///
    /// # Errors
    /// [`EmbeddedRefused`], naming the cause; nothing is left running.
    pub fn start(launch: EmbeddedLaunch) -> Result<Self, EmbeddedRefused> {
        let EmbeddedLaunch {
            app_data_dir,
            profile,
            identity,
        } = launch;
        identity
            .transport_identity()
            .map_err(|_| EmbeddedRefused::IdentityRejected)?;
        let boundary = TrustBoundary::new(&app_data_dir)?;
        let paths = ProfilePaths::resolve_embedded(&profile, boundary)?;
        let config = ProfileConfig::load(&paths)?;
        if config.runtime.deployment != Deployment::EmbeddedAndroid {
            return Err(EmbeddedRefused::NotEmbedded);
        }
        let lock = ProfileLock::acquire(&paths, LOCK_WAIT)?;
        // Read under the lock, so no other host is writing it. A present
        // overlay that cannot be trusted refuses the start, as the trust
        // overlay's does: read as absent, it would turn the person's
        // explicit opt-in off without their knowing.
        let authored = config.runtime.android.availability_mode;
        let started_in = availability_overlay::effective(
            authored,
            availability_overlay::read_within(
                &availability_overlay::path_for(&paths),
                paths.boundary(),
            )?,
        );
        let executor = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(WORKERS)
            .thread_name("interweave-embedded")
            .enable_all()
            .build()
            .map_err(|e| EmbeddedRefused::Internal(e.to_string()))?;
        let composed = executor.block_on(ComposedRuntime::start(
            &identity,
            &config,
            CompositionOptions::from_profile(&config, &paths),
        ))?;
        Ok(Self {
            sessions: composed.sessions(),
            composed: Some(composed),
            handle: executor.handle().clone(),
            executor: Some(executor),
            paths,
            log_level: config.observability.log_level,
            authored,
            started_in,
            availability: tokio::sync::watch::Sender::new(started_in),
            availability_writes: std::sync::Mutex::new(()),
            lock: Some(lock),
        })
    }

    /// The in-process binding: `LocalDataSession` and `LocalAdminPort`,
    /// what the app's clients open their sessions on.
    #[must_use]
    pub fn binding(&self) -> InProcessBinding {
        self.sessions.clone()
    }

    /// The executor the binding's futures are driven on.
    #[must_use]
    pub fn runtime(&self) -> tokio::runtime::Handle {
        self.handle.clone()
    }

    /// The addresses the runtime bound at start, as the substrate
    /// reported them: for the Service's diagnostics, and for a peer
    /// that must be told where this one listens.
    #[must_use]
    pub fn listening(&self) -> Vec<String> {
        self.composed
            .as_ref()
            .map(|composed| composed.listening().to_vec())
            .unwrap_or_default()
    }

    /// What the platform's network callbacks hand in
    /// (`human-client-android.md` "network callbacks", §20 step 5): a
    /// SNAPSHOT of every usable address the platform reports, never a
    /// delta, and an empty view when it reports none -- offline. Pass a
    /// view when the addresses change; a new network whose addresses
    /// equal the old ones is no change, and the connections that
    /// survived it prove themselves by their own keepalives. Until the
    /// first view the runtime knows the host's addresses from its
    /// wildcard listeners alone, which on Android poll every 10 s and may
    /// see nothing at all; with a view, a hand-over is seen at once.
    ///
    /// Callable from any thread, and does not wait: it is
    /// `ComposedRuntime::network_changed`, a snapshot slot where the
    /// latest view replaces one the runtime has not yet read
    /// (`the_latest_view_replaces_one_not_yet_read` pins the slot). After
    /// [`stop`](Self::stop) there is no
    /// host to call it on; while the runtime is stopping, a view goes
    /// nowhere.
    pub fn network_changed(&self, view: NetworkView) {
        if let Some(composed) = self.composed.as_ref() {
            composed.network_changed(view);
        }
    }

    /// The effective availability mode now: the person's choice when
    /// there is one, else `config.yaml`'s authored mode. What
    /// `AndroidRuntimeConfig::background_restart_requires_user_authentication`
    /// and the diagnostic take. Before a host starts, the same reading is
    /// `availability_overlay::effective_availability_mode`.
    #[must_use]
    pub fn availability(&self) -> AvailabilityMode {
        *self.availability.borrow()
    }

    /// Record the person's Stay-reachable choice: `Some` writes the
    /// overlay entry, `None` REMOVES it -- off is the entry's absence,
    /// never `foreground-only` written down (ADR-0041 A 2026-10-10), and
    /// [`StayReachable`] is the only value there is to write. The one
    /// write path; `config.yaml` is never written. When the effective
    /// mode moves away from the one the host started in,
    /// [`wait_shutdown_requested`](Self::wait_shutdown_requested) returns
    /// [`Ended::AvailabilityChanged`]; a choice that leaves it where it
    /// was -- on, over an authored `stay-reachable` -- changes nothing a
    /// waiter sees.
    ///
    /// BLOCKS on the file write; callable from any thread.
    ///
    /// # Errors
    /// [`EmbeddedRefused`] when the write or the removal failed. What is
    /// then on disk is read back and is what
    /// [`availability`](Self::availability) answers: a failure before the rename or the
    /// unlink leaves the previous choice, one after it (the directory
    /// sync) leaves the new one, its name perhaps not durable.
    pub fn set_availability(&self, choice: Option<StayReachable>) -> Result<(), EmbeddedRefused> {
        let _serial = self
            .availability_writes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = availability_overlay::path_for(&self.paths);
        let written = availability_overlay::write_within(&path, choice, self.paths.boundary());
        let on_disk = match &written {
            Ok(()) => Ok(choice),
            Err(_) => availability_overlay::read_within(&path, self.paths.boundary()),
        };
        if let Ok(chosen) = on_disk {
            let mode = availability_overlay::effective(self.authored, chosen);
            self.availability.send_if_modified(|current| {
                let moved = *current != mode;
                *current = mode;
                moved
            });
        }
        written.map_err(EmbeddedRefused::from)
    }

    /// The profile's `observability.log_level`, for [`log_admits`].
    #[must_use]
    pub fn log_level(&self) -> LogLevel {
        self.log_level
    }

    /// The profile's paths: the boundary and the root every private
    /// directory of this host must lie under -- for the human store's
    /// opener, which opens under them.
    #[must_use]
    pub fn paths(&self) -> &ProfilePaths {
        &self.paths
    }

    /// Wait until the Service should stop the host:
    /// [`Ended::ShutdownRequested`] for a request -- an admin port's
    /// (`AdminPort::shutdown`), or the owner's own
    /// [`request_shutdown`](Self::request_shutdown) from another thread,
    /// the way out for a Service the platform stops while a thread of it
    /// waits here, since [`stop`](Self::stop) cannot be called while the
    /// host is borrowed -- or [`Ended::RuntimeEnded`] when the runtime
    /// ended on its own, its substrate gone, so a Service does not keep a
    /// dead runtime in the foreground (`ComposedRuntime::wait_end`) -- or
    /// [`Ended::AvailabilityChanged`] once the effective availability
    /// mode differs from the one the host started in, at once if it
    /// already does. A request wins over the other two, and a runtime's
    /// end over a changed mode, when more than one holds. The Service
    /// answers any of them by calling `stop`, with the grace asked for or
    /// its own. `RuntimeEnded` IS the failure's report: `stop` after it
    /// releases the lock and answers the dropped count as after any other
    /// end (`a_runtime_that_ends_on_its_own_releases_the_waiter`), and a
    /// fresh host may be started, in the new mode after an
    /// `AvailabilityChanged`.
    ///
    /// BLOCKS; call it off any async context.
    #[must_use]
    pub fn wait_shutdown_requested(&self) -> Ended {
        match (self.executor.as_ref(), self.composed.as_ref()) {
            (Some(executor), Some(composed)) => {
                let mut availability = self.availability.subscribe();
                let started_in = self.started_in;
                executor.block_on(async {
                    tokio::select! {
                        biased;
                        ended = composed.wait_end() => match ended {
                            ComposedEnded::ShutdownRequested(request) => {
                                Ended::ShutdownRequested(request)
                            }
                            ComposedEnded::RuntimeEnded => Ended::RuntimeEnded,
                        },
                        // The sender is the host's own, alive while this
                        // borrow is: `Err` cannot arrive.
                        Ok(mode) = availability.wait_for(|mode| *mode != started_in) => {
                            Ended::AvailabilityChanged(*mode)
                        }
                    }
                })
            }
            // Only `stop` and `Drop` take them, and both consume the host.
            _ => Ended::RuntimeEnded,
        }
    }

    /// End the runtime's driver as if its substrate had gone, so a test
    /// reaches [`Ended::RuntimeEnded`] through the host. TEST BUILDS
    /// ONLY.
    #[cfg(feature = "test-hooks")]
    pub fn end_runtime_for_test(&self) {
        if let (Some(executor), Some(composed)) = (self.executor.as_ref(), self.composed.as_ref()) {
            let _ = executor.block_on(composed.end_driver());
        }
    }

    /// Ask, as the runtime's owner, that it stop within `grace`: what an
    /// admin port's shutdown does, through a port the host opens with
    /// that one capability. A thread waiting in
    /// [`wait_shutdown_requested`](Self::wait_shutdown_requested)
    /// returns with it (`a_platform_stop_releases_the_waiter`). The FIRST
    /// request stands: if an admin port asked already, the waiter sees
    /// that request's grace, not this one's.
    ///
    /// BLOCKS; call it off any async context.
    ///
    /// # Errors
    /// [`EmbeddedRefused::Internal`] if the binding cannot open the port.
    pub fn request_shutdown(&self, grace: Duration) -> Result<(), EmbeddedRefused> {
        let capabilities = std::collections::BTreeSet::from([AdminCapability::Shutdown]);
        self.handle
            .block_on(async {
                let port = self.sessions.admin(capabilities).await?;
                port.shutdown(grace).await
            })
            .map_err(|e| EmbeddedRefused::Internal(format!("{e:?}")))
    }

    /// Stop the runtime, letting exchanges in flight settle for `grace`,
    /// then release the profile lock. Answers the neutral events the
    /// runtime dropped over its whole life, its shutdown backlog
    /// included: a diagnostic count, nothing a person acts on.
    ///
    /// BLOCKS; call it off any async context.
    ///
    /// # Errors
    /// [`EmbeddedRefused::Internal`] if the runtime's driver failed; the
    /// lock is released all the same.
    pub fn stop(mut self, grace: Duration) -> Result<u64, EmbeddedRefused> {
        let stopped = match (self.executor.as_ref(), self.composed.take()) {
            (Some(executor), Some(composed)) => executor
                .block_on(composed.stop_within(grace))
                .map_err(|e| EmbeddedRefused::Internal(format!("{e:?}"))),
            _ => Ok(0),
        };
        drop(self.executor.take());
        drop(self.lock.take());
        stopped
    }
}

impl Drop for EmbeddedHost {
    /// Dropped without [`stop`](EmbeddedHost::stop): the runtime's tasks
    /// are cancelled WITHOUT WAITING -- a drop may happen anywhere, inside
    /// an async context included, where blocking would panic -- and the
    /// lock released after. A worker mid-write when the drop came may
    /// finish that write after the release, so the Service stops a host
    /// with `stop`, which waits, and a drop is the fallback.
    fn drop(&mut self) {
        drop(self.composed.take());
        if let Some(executor) = self.executor.take() {
            executor.shutdown_background();
        }
        drop(self.lock.take());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory absent or closed is the person's to act on; any other
    /// I/O failure is not a directory's fault.
    #[test]
    fn an_io_failure_is_a_directory_only_when_absent_or_closed() {
        let io = |kind| EmbeddedRefused::from(PersistError::Io(std::io::Error::from(kind)));
        assert!(matches!(
            io(std::io::ErrorKind::NotFound),
            EmbeddedRefused::DirectoryRefused(_)
        ));
        assert!(matches!(
            io(std::io::ErrorKind::PermissionDenied),
            EmbeddedRefused::DirectoryRefused(_)
        ));
        assert!(matches!(
            io(std::io::ErrorKind::StorageFull),
            EmbeddedRefused::Internal(_)
        ));
    }
}
