// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The facade on the Service's side: one thread for the process, turning
//! on the embedded runtime's own executor ([`EmbeddedHost::runtime`]), so
//! the process runs one executor, not two.
//!
//! Why its own thread, as on the desktop: a session whose events go
//! undrained loses its lease, so the facade turns on its own schedule
//! whatever the Activity is doing -- and here, whether there is an
//! Activity at all.
//!
//! [`EmbeddedHost::runtime`]: interweave_transport_embedded::EmbeddedHost::runtime

use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use interweave_human_app_core::{Command, FacadeSide, Update};
use interweave_human_store::StoreError;
use interweave_human_transport_client::ClientEvent;
use interweave_local_client_api::{AdminBinding, DataSessionBinding};
use tokio::sync::{mpsc, oneshot};

use crate::hub::{Hub, ToView};

/// How long the loop waits for a command before it turns anyway.
const POLL: Duration = Duration::from_millis(100);

/// The facade's state now, as the updates that would have set it: what a
/// view that attaches late needs beside the store's rows.
fn snapshot<B: DataSessionBinding, A: AdminBinding>(side: &FacadeSide<B, A>) -> [Update; 3] {
    let client = side.client();
    [
        Update::Client(ClientEvent::Session(client.session_state().clone())),
        Update::Client(ClientEvent::Connectivity(client.connectivity())),
        Update::Diagnostics(client.diagnostics()),
    ]
}

/// Why the facade did not start.
#[derive(Debug)]
pub enum SpawnError {
    /// Its thread could not be started.
    Thread(std::io::Error),
    /// Its store's pending rows could not be read.
    Store(StoreError),
}

/// Why the loop woke.
enum Woke {
    /// A command arrived.
    Command(Command),
    /// The loop is asked to close, or nothing can command it any more.
    Closed,
    /// Nothing arrived within [`POLL`]: a turn is due anyway.
    Poll,
}

/// The running facade loop.
pub struct FacadeLoop {
    close: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl FacadeLoop {
    /// Build the facade side with `make` on a thread of its own -- a
    /// session is not `Send`, so it lives where it is made -- and run it
    /// there, its futures driven on `executor`, its output to `hub`. The
    /// hub hears the facade running before this returns.
    ///
    /// # Errors
    /// When the thread cannot be started, or `make` fails: the store's
    /// pending rows could not be read. Nothing runs then.
    pub fn spawn<B, A>(
        executor: tokio::runtime::Handle,
        make: impl FnOnce() -> Result<FacadeSide<B, A>, StoreError> + Send + 'static,
        hub: &Arc<Hub>,
    ) -> Result<Self, SpawnError>
    where
        B: DataSessionBinding + 'static,
        A: AdminBinding + 'static,
    {
        let (commands, mut waiting) = mpsc::unbounded_channel();
        let (close, mut closed) = oneshot::channel();
        let (report, reported) = std::sync::mpsc::sync_channel(1);
        let on_thread = Arc::clone(hub);
        let spawned = std::thread::Builder::new()
            .name("human-facade".to_owned())
            .spawn(move || {
                let hub = on_thread;
                let mut side = match make() {
                    Ok(side) => side,
                    Err(e) => {
                        let _ = report.send(Err(e));
                        return;
                    }
                };
                hub.set_running(Some(commands));
                let _ = report.send(Ok(()));
                let start = Instant::now();
                let now = || u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
                executor.block_on(async {
                    loop {
                        if hub.wants_listing() {
                            hub.listed(match side.listing() {
                                Ok(listing) => ToView::Update(Box::new(Update::Listed(listing))),
                                Err(_) => ToView::ListingFailed,
                            });
                            // A view attaching to a facade that has run
                            // for a while missed the events that set its
                            // state: they went out with no view to take
                            // them. It is told the session, connectivity
                            // and counters as they are now. NOT a peer's
                            // path: the facade keeps no current path to
                            // replay, so a route indicator shows nothing
                            // until the path next changes (carried).
                            for update in snapshot(&side) {
                                hub.update(update);
                            }
                        }
                        let woke = tokio::select! {
                            command = waiting.recv() => command.map_or(Woke::Closed, Woke::Command),
                            _ = &mut closed => Woke::Closed,
                            () = tokio::time::sleep(POLL) => Woke::Poll,
                        };
                        // Every command waiting now rather than one per
                        // turn, as the desktop does: a burst of reads is
                        // carried out before the next turn.
                        let mut batch = match woke {
                            Woke::Closed => break,
                            Woke::Command(command) => vec![command],
                            Woke::Poll => Vec::new(),
                        };
                        while let Ok(more) = waiting.try_recv() {
                            batch.push(more);
                        }
                        for command in batch {
                            for update in side.execute(command, now()).await {
                                hub.update(update);
                            }
                        }
                        for update in side.turn(now()).await {
                            hub.update(update);
                        }
                    }
                    side.close().await;
                });
                hub.set_running(None);
            });
        let thread = spawned.map_err(SpawnError::Thread)?;
        match reported.recv() {
            Ok(Ok(())) => Ok(Self {
                close: Some(close),
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(SpawnError::Store(e))
            }
            // The thread ended without a word: it panicked in `make`.
            Err(_) => {
                let _ = thread.join();
                Err(SpawnError::Thread(std::io::Error::other(
                    "the facade's thread ended while it was built",
                )))
            }
        }
    }

    /// Close the session -- the lease is released -- and wait for the
    /// thread, at most `wait`. True when it ended in time.
    #[must_use]
    pub fn close(mut self, wait: Duration) -> bool {
        if let Some(close) = self.close.take() {
            let _ = close.send(());
        }
        let Some(thread) = self.thread.take() else {
            return true;
        };
        let deadline = Instant::now() + wait;
        while !thread.is_finished() {
            if Instant::now() >= deadline {
                // Left running: the runtime's stop closes the session
                // under it, and the process may end before it does.
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = thread.join();
        true
    }
}
