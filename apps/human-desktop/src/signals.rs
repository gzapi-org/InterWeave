// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! SIGTERM and SIGINT end the window as closing it does: the session is
//! closed and the lease released, never the daemon.

use tokio::signal::unix::{SignalKind, signal};

/// Call `then` once, from a thread of its own, when SIGTERM or SIGINT
/// arrives.
pub(crate) fn on_terminate(then: impl FnOnce() + Send + 'static) {
    if let Err(e) = std::thread::Builder::new()
        .name("human-signals".to_owned())
        .spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                eprintln!("human-desktop: signals cannot be watched; close the window to stop");
                return;
            };
            let arrived = runtime.block_on(async {
                let (Ok(mut term), Ok(mut int)) = (
                    signal(SignalKind::terminate()),
                    signal(SignalKind::interrupt()),
                ) else {
                    return false;
                };
                tokio::select! {
                    _ = term.recv() => true,
                    _ = int.recv() => true,
                }
            });
            if arrived {
                then();
            } else {
                eprintln!("human-desktop: signals cannot be watched; close the window to stop");
            }
        })
    {
        eprintln!("human-desktop: signals cannot be watched: {e}");
    }
}
