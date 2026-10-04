// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! SIGTERM and SIGINT end the window as closing it does: the session is
//! closed and the lease released, never the daemon.

use std::sync::mpsc;

use tokio::signal::unix::{SignalKind, signal};

/// Call `then` once, from a thread of its own, when SIGTERM or SIGINT
/// arrives. Returns once the handlers are installed, so a signal sent
/// after it -- once the lease is asked for, say -- is caught rather than
/// given its default action. The thread installs them itself: a thread
/// that could not start must leave the defaults, not handlers nobody
/// answers.
pub(crate) fn on_terminate(then: impl FnOnce() + Send + 'static) {
    let (installed, wait) = mpsc::sync_channel::<Result<(), String>>(1);
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
            let arrived = runtime.block_on(async {
                let (mut term, mut int) = match (
                    signal(SignalKind::terminate()),
                    signal(SignalKind::interrupt()),
                ) {
                    (Ok(term), Ok(int)) => (term, int),
                    (Err(e), _) | (_, Err(e)) => {
                        let _ = installed.send(Err(e.to_string()));
                        return false;
                    }
                };
                let _ = installed.send(Ok(()));
                tokio::select! {
                    _ = term.recv() => {}
                    _ = int.recv() => {}
                }
                true
            });
            if arrived {
                then();
            }
        });
    let outcome = match spawned {
        Ok(_) => wait
            .recv()
            .unwrap_or_else(|_| Err("the watching thread ended".to_owned())),
        Err(e) => Err(e.to_string()),
    };
    if let Err(e) = outcome {
        eprintln!("human-desktop: signals cannot be watched, close the window to stop: {e}");
    }
}
