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
use std::time::Duration;

use interweave_ipc_server::{
    KeepalivePolicy, Limits, ServerConfig, SocketPaths, WRITE_STALL, bind, remove_stale_socket,
    serve,
};
use interweave_profile_config::lock::{DAEMON_LOCK_WAIT, ProfileLock};
use interweave_profile_config::runtime::Deployment;
use interweave_profile_config::sections::LogLevel;
use interweave_profile_config::{ProfileConfig, ProfilePaths, XdgRoots};
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_composition::{ComposedRuntime, CompositionOptions};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::oneshot;

use crate::cli::Args;

/// The grace an `admin.shutdown` naming none is given.
const DEFAULT_SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

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
    // The lock is held: no other daemon of this profile serves these.
    for socket in [&sockets.data, &sockets.admin] {
        remove_stale_socket(socket).map_err(|e| refused("a socket's place")(&e))?;
    }
    let runtime = ComposedRuntime::start(
        &identity,
        &profile,
        CompositionOptions::from_profile(&profile, &paths),
    )
    .await
    .map_err(|e| refused("the runtime")(&e))?;
    let listeners = bind(&sockets).map_err(|e| refused("the IPC sockets")(&e))?;
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
    let server = tokio::spawn(serve(listeners, runtime.sessions(), config, async {
        let _ = server_stopped.await;
    }));
    tracing::info!(data = %sockets.data.display(), "serving");

    wait_for_stop(&runtime).await;

    tracing::info!("stopping");
    let _ = stop_server.send(());
    // Every connection closed, so every session closed and every lease
    // released, before the runtime goes.
    let server_outcome = server.await;
    let stopped = runtime.stop().await;
    for socket in [&sockets.data, &sockets.admin] {
        match std::fs::remove_file(socket) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!(socket = %socket.display(), error = %e, "not unlinked"),
        }
    }
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

/// Until SIGINT, SIGTERM, or an admin port asks.
async fn wait_for_stop(runtime: &ComposedRuntime) {
    let (Ok(mut interrupt), Ok(mut terminate)) = (
        signal(SignalKind::interrupt()),
        signal(SignalKind::terminate()),
    ) else {
        tracing::error!("cannot watch for signals; stopping");
        return;
    };
    tokio::select! {
        _ = interrupt.recv() => tracing::info!("SIGINT"),
        _ = terminate.recv() => tracing::info!("SIGTERM"),
        request = runtime.shutdown_requested() => {
            if let Some(request) = request {
                tracing::info!(
                    port = request.port.as_str(),
                    grace_ms = u64::try_from(request.grace.as_millis()).unwrap_or(u64::MAX),
                    "an admin port asked for shutdown"
                );
            } else {
                tracing::warn!("nothing is left that could ask for shutdown");
            }
        }
    }
}

/// Logging to stderr at the profile's level, and at no other: no
/// environment variable overrides it (plan §16 (11)).
fn init_logging(level: LogLevel) {
    let level = match level {
        LogLevel::Error => tracing::Level::ERROR,
        LogLevel::Warn => tracing::Level::WARN,
        LogLevel::Info => tracing::Level::INFO,
        LogLevel::Debug => tracing::Level::DEBUG,
    };
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_max_level(level)
        .try_init();
}
