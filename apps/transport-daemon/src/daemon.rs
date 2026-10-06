// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The daemon's lifecycle, in `lifecycle.md` §Daemon lifecycle's order.
//!
//! Start: load and validate the profile; take the profile lock (a second
//! daemon fails here); load the key (a missing one is fatal unless
//! `--create-identity` and no key file); clear this uid's stale sockets
//! and bind both; compose and start the runtime; serve IPC.
//! Stop, on SIGINT, SIGTERM or an admin port's request: stop the server
//! (every connection closed, every lease released), stop the runtime
//! (settle, close the Swarm), unlink both sockets, release the lock --
//! released, never unlinked.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use interweave_ipc_server::{
    Counters, KeepalivePolicy, Limits, ServerConfig, SocketPaths, WRITE_STALL,
    bind_replacing_stale, serve,
};
use interweave_profile_config::lock::{DAEMON_LOCK_WAIT, ProfileLock};
use interweave_profile_config::runtime::Deployment;
use interweave_profile_config::sections::LogLevel;
use interweave_profile_config::{ProfileConfig, ProfilePaths, XdgRoots};
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_composition::{
    AUDIT_TARGET, ComposedRuntime, CompositionOptions, SHUTDOWN_GRACE,
};
use tokio::signal::unix::{Signal, SignalKind, signal};
use tokio::sync::oneshot;

use crate::cli::Args;

/// The grace an `admin.shutdown` naming none is given: the substrate's
/// own default, so a signal and a grace-less request stop alike.
const DEFAULT_SHUTDOWN_GRACE: Duration = SHUTDOWN_GRACE;

/// A request's deadline when it names none (`TRANSPORT.md`: 10 s).
const COMMAND_DEADLINE: Duration = Duration::from_secs(10);

/// Why the daemon refused to start or stopped badly: printed, exit 1.
#[derive(Debug)]
pub(crate) struct Refused(String);

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn refused(what: &str) -> impl FnOnce(&dyn fmt::Display) -> Refused + '_ {
    move |e| Refused(format!("{what}: {e}"))
}

/// Run the daemon for `args.profile` until it is asked to stop.
///
/// # Errors
/// Every fatal start condition of `failure-model.md` (the profile, the
/// lock, the key, a socket's place), and a runtime that failed to start
/// or stop.
pub(crate) async fn run(args: Args) -> Result<(), Refused> {
    // Registered before anything is taken, so a signal during start is
    // held and answered by a clean stop once serving, rather than the
    // default action killing the process with its sockets bound.
    let signals = Signals::register().map_err(|e| refused("the signal handlers")(&e))?;
    let roots = XdgRoots::from_env().map_err(|e| refused("the XDG directories")(&e))?;
    let paths = ProfilePaths::resolve(&args.profile, &roots)
        .map_err(|e| refused("the profile's paths")(&e))?;
    let profile = ProfileConfig::load(&paths).map_err(|e| refused("the profile")(&e))?;
    if profile.runtime.deployment == Deployment::EmbeddedAndroid {
        return Err(Refused(
            "the profile is embedded-android: it runs inside the app, not as a daemon".into(),
        ));
    }
    init_logging(profile.observability.log_level);

    let lock = ProfileLock::acquire(&paths, DAEMON_LOCK_WAIT)
        .map_err(|e| refused("the profile lock (is a daemon already running?)")(&e))?;
    let identity = identity(&profile.identity.key_file_in(&paths), args.create_identity)?;
    let peer = identity
        .transport_identity()
        .map_err(|e| refused("the identity")(&e))?;
    tracing::info!(profile = paths.profile(), peer = peer.as_str(), "starting");

    let sockets = sockets(&paths)?;
    // BOUND BEFORE THE RUNTIME STARTS (lifecycle.md steps 4 then 5), so a
    // fatal socket condition refuses the start before the profile is on
    // the network. The lock is held: a stale socket of this uid that no
    // process serves is replaced.
    let listeners = bind_replacing_stale(&sockets)
        .await
        .map_err(|e| refused("the IPC sockets")(&e))?;
    let runtime = match ComposedRuntime::start(
        &identity,
        &profile,
        CompositionOptions::from_profile(&profile, &paths),
    )
    .await
    {
        Ok(runtime) => runtime,
        Err(e) => {
            drop(listeners);
            unlink(&sockets);
            return Err(refused("the runtime")(&e));
        }
    };
    if let Some(e) = listeners.admin_error() {
        // ADR-0037: the admin socket failing does not take data down.
        tracing::warn!(error = %e, "the admin socket is unavailable; serving data only");
    }
    let config = ServerConfig {
        peer,
        limits: Limits {
            max_clients: usize::try_from(profile.ipc.max_clients).unwrap_or(usize::MAX),
            max_admin_clients: usize::try_from(profile.ipc.max_admin_clients).unwrap_or(usize::MAX),
        },
        keepalive: KeepalivePolicy {
            enabled: profile.ipc.keepalive.enabled,
            required_for_lease: profile.ipc.keepalive.require_for_endpoint_lease,
            interval: Duration::from_millis(u64::from(profile.ipc.keepalive.interval_ms)),
            response_timeout: Duration::from_millis(u64::from(
                profile.ipc.keepalive.response_timeout_ms,
            )),
            max_missed: profile.ipc.keepalive.max_missed,
        },
        shutdown_grace: DEFAULT_SHUTDOWN_GRACE,
        command_deadline: COMMAND_DEADLINE,
        write_stall: WRITE_STALL,
    };
    let (stop_server, server_stopped) = oneshot::channel::<()>();
    let mut server = tokio::spawn(serve(listeners, runtime.sessions(), config, async {
        let _ = server_stopped.await;
    }));
    tracing::info!(data = %sockets.data.display(), "serving");

    let (grace, ended_early) = signals.wait_for_stop(&runtime, &mut server).await;

    tracing::info!(grace_ms = millis(grace), "stopping");
    let _ = stop_server.send(());
    // Every connection closed, so every session closed and every lease
    // released, before the runtime goes.
    let server_outcome = match ended_early {
        Some(outcome) => outcome,
        None => server.await,
    };
    let stopped = runtime.stop_within(grace).await;
    unlink(&sockets);
    // Released, never unlinked (ADR-0028, A 2026-09-28).
    drop(lock);
    server_outcome.map_err(|e| refused("the IPC server")(&e))?;
    let dropped = stopped.map_err(|e| Refused(format!("the runtime's stop: {e:?}")))?;
    tracing::info!(events_dropped = dropped, "stopped");
    Ok(())
}

/// Load the key, or create it when asked and none exists: never a silent
/// new `PeerId` (plan §16 (12)).
fn identity(key_file: &Path, create: bool) -> Result<ProfileIdentity, Refused> {
    match std::fs::symlink_metadata(key_file) {
        Ok(_) => ProfileIdentity::load(key_file).map_err(|e| refused("the identity key")(&e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if !create {
                return Err(Refused(format!(
                    "no identity key at {}: pass --create-identity to create one, or \
                     restore it (transportctl identity restore)",
                    key_file.display()
                )));
            }
            let identity = ProfileIdentity::generate();
            identity
                .save(key_file)
                .map_err(|e| refused("creating the identity key")(&e))?;
            Ok(identity)
        }
        Err(e) => Err(refused("the identity key")(&e)),
    }
}

/// Both sockets and the directory they share.
fn sockets(paths: &ProfilePaths) -> Result<SocketPaths, Refused> {
    let data = paths
        .data_socket()
        .map_err(|e| refused("the data socket's path")(&e))?;
    let admin = paths
        .admin_socket()
        .map_err(|e| refused("the admin socket's path")(&e))?;
    let run_dir: PathBuf = data
        .parent()
        .ok_or_else(|| Refused("the data socket has no directory".into()))?
        .to_path_buf();
    Ok(SocketPaths {
        run_dir,
        data,
        admin,
    })
}

/// SIGINT and SIGTERM, watched from the start.
struct Signals {
    interrupt: Signal,
    terminate: Signal,
}

impl Signals {
    fn register() -> std::io::Result<Self> {
        Ok(Self {
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
        })
    }

    /// Until SIGINT, SIGTERM, an admin port's request, or the server
    /// ending on its own: the grace the stop is given -- the admin's, or
    /// the default -- and the server's outcome if it had already ended (a
    /// finished task must not be awaited twice).
    async fn wait_for_stop(
        mut self,
        runtime: &ComposedRuntime,
        server: &mut tokio::task::JoinHandle<Arc<Counters>>,
    ) -> (
        Duration,
        Option<Result<Arc<Counters>, tokio::task::JoinError>>,
    ) {
        tokio::select! {
            _ = self.interrupt.recv() => tracing::info!("SIGINT"),
            _ = self.terminate.recv() => tracing::info!("SIGTERM"),
            request = runtime.shutdown_requested() => {
                if let Some(request) = request {
                    tracing::info!(
                        port = request.port.as_str(),
                        grace_ms = millis(request.grace),
                        "an admin port asked for shutdown"
                    );
                    return (request.grace, None);
                }
                tracing::warn!("nothing is left that could ask for shutdown");
            }
            // It ends only when told to; ending first is a failure, and
            // a daemon serving no IPC is not left running.
            outcome = &mut *server => {
                tracing::error!("the IPC server ended on its own");
                return (SHUTDOWN_GRACE, Some(outcome));
            }
        }
        (SHUTDOWN_GRACE, None)
    }
}

fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// Unlink both sockets, as their binder: a missing one is fine.
fn unlink(sockets: &SocketPaths) {
    for socket in [&sockets.data, &sockets.admin] {
        match std::fs::remove_file(socket) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!(socket = %socket.display(), error = %e, "not unlinked"),
        }
    }
}

/// The prefix every first-party library target begins with: the crates'
/// module paths (`interweave_*`) and the named targets
/// (`interweave::audit`, `interweave::connectivity`).
const FIRST_PARTY: &str = "interweave";

/// This binary's own target, which does not begin with [`FIRST_PARTY`]:
/// the crate is `transport_daemon`, so its `starting` and `serving`
/// lines would be capped with the third parties without it.
const THIS_DAEMON: &str = env!("CARGO_CRATE_NAME");

/// Logging to stderr at the profile's level, and at no other: no
/// environment variable overrides it (plan §16 (11)).
///
/// EXCEPT THE AUDIT TARGET, which is written at INFO whatever the level.
/// LOCAL-IPC.md requires each trust change to be in the daemon's log so
/// it can be audited, unconditionally; a profile at `warn` or `error`
/// would otherwise apply the change and leave no record of it
/// (`a_trust_change_is_in_the_log_at_every_level`, desktop-e2e).
///
/// AND EVERY THIRD-PARTY TARGET AT `warn` AT MOST (`observability.md`
/// §Logs, A 2026-10-06): libp2p and the stack beneath it log addresses
/// at `debug`, which ADR-0052 rule 5 keeps out of any log, and nothing
/// here checks what they write. So only this repository's own targets --
/// the libraries' `interweave…` and this binary's own -- follow the
/// profile's level; the rest are capped, whichever crate they come from,
/// a new dependency's included
/// (`a_peers_restart_is_in_the_connectivity_log_and_third_party_debug_is_not`,
/// desktop-e2e).
fn init_logging(level: LogLevel) {
    use tracing_subscriber::filter::Targets;
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;
    let level = match level {
        LogLevel::Error => tracing::Level::ERROR,
        LogLevel::Warn => tracing::Level::WARN,
        LogLevel::Info => tracing::Level::INFO,
        LogLevel::Debug => tracing::Level::DEBUG,
    };
    // The writer passes whatever the filter does, and no less than INFO,
    // so the audit target is not cut before the filter can admit it.
    let widest = if level == tracing::Level::DEBUG {
        tracing::Level::DEBUG
    } else {
        tracing::Level::INFO
    };
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_max_level(widest)
        .finish()
        .with(
            Targets::new()
                .with_default(std::cmp::min(level, tracing::Level::WARN))
                .with_target(FIRST_PARTY, level)
                .with_target(THIS_DAEMON, level)
                .with_target(AUDIT_TARGET, tracing::Level::INFO),
        )
        .try_init();
}
