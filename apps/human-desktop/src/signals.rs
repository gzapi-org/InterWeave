// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! SIGTERM and SIGINT end the window as closing it does: the session is
//! closed and the lease released, never the daemon.

use std::sync::mpsc;

use tokio::signal::unix::{Signal, SignalKind, signal};

/// The next delivery of `watched`, or never when it is not watched.
async fn next(watched: &mut Option<(SignalKind, Signal)>) -> SignalKind {
    match watched {
        Some((kind, stream)) => {
            stream.recv().await;
            *kind
        }
        None => std::future::pending().await,
    }
}

/// Call `then` once, from a thread of its own, when SIGTERM or SIGINT
/// arrives. Returns once the handlers
/// are installed, so a signal sent after it -- once the lease is asked
/// for, say -- is caught rather than given its default action. Each
/// signal is installed on its own: one that cannot be watched is
/// reported and the other still works, and an installed handler is
/// always answered. The thread installs them itself, so a thread that
/// could not start leaves the defaults.
pub(crate) fn on_terminate(then: impl FnOnce() + Send + 'static) {
    let (installed, wait) = mpsc::sync_channel::<Result<Vec<String>, String>>(1);
    let spawned = std::thread::Builder::new()
        .name("human-signals".to_owned())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(e) => {
                    let _ = installed.send(Err(e.to_string()));
                    return;
                }
            };
            runtime.block_on(async {
                let mut unwatched = Vec::new();
                let mut watch = |kind: SignalKind, name: &str| match signal(kind) {
                    Ok(stream) => Some((kind, stream)),
                    Err(e) => {
                        unwatched.push(format!("{name}: {e}"));
                        None
                    }
                };
                let mut term = watch(SignalKind::terminate(), "SIGTERM");
                let mut int = watch(SignalKind::interrupt(), "SIGINT");
                if term.is_none() && int.is_none() {
                    let _ = installed.send(Err(unwatched.join("; ")));
                    return;
                }
                let _ = installed.send(Ok(unwatched));
                tokio::select! {
                    _ = next(&mut term) => {}
                    _ = next(&mut int) => {}
                }
                then();
            });
        });
    let outcome = match spawned {
        Ok(_) => wait
            .recv()
            .unwrap_or_else(|_| Err("the watching thread ended".to_owned())),
        Err(e) => Err(e.to_string()),
    };
    match outcome {
        Ok(unwatched) if unwatched.is_empty() => {}
        Ok(unwatched) => eprintln!(
            "human-desktop: not watched, close the window to stop: {}",
            unwatched.join("; ")
        ),
        Err(e) => {
            eprintln!("human-desktop: signals cannot be watched, close the window to stop: {e}");
        }
    }
}
