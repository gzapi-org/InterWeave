// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The facade side on a thread of its own, with its own runtime.
//!
//! Why not the window's thread: a session whose events go undrained stops
//! answering keepalive pings and loses its lease (`ipc-client`), so the
//! facade turns on its own schedule, whatever the window is doing. And
//! why a multi-thread runtime with one worker: the IPC client's reader
//! task then runs on that worker, so a store call that blocks this thread
//! (an fsync) cannot starve the pings either.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use interweave_human_app_core::{Command, FacadeSide, Update};
use interweave_human_store::StoreError;
use interweave_local_client_api::{AdminBinding, DataSessionBinding};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

/// How many messages wait for the window at most. When the window falls
/// that far behind, the facade thread waits for it before it hands over
/// more: it then stops draining, so what is still arriving stays in the
/// daemon's bounded endpoint queue, and what was drained is in the store
/// already. A window that stalls costs the process this many messages of
/// memory, not one per message sent to it -- and, stalled past the
/// keepalive miss threshold under sustained traffic, its lease, as any
/// session that leaves its events undrained (LOCAL-IPC.md); it then
/// reconnects. Bounded memory is the side of that trade CLAUDE.md
/// section 6 takes.
pub const TO_WINDOW: usize = 256;

/// How long the facade waits for a command before it turns anyway.
const POLL: Duration = Duration::from_millis(100);

/// How often whether a daemon serves the profile is asked again.
const DAEMON_PROBE: Duration = Duration::from_secs(1);

/// What the window's side sends.
#[derive(Debug)]
pub enum ToFacade {
    /// Carry out a command.
    Command(Command),
    /// Close the session and stop. The lease is released; the daemon is
    /// not asked to stop (ADR-0040).
    Close,
}

/// What the facade's side sends back.
#[derive(Debug)]
pub enum FromFacade {
    /// An update for the model side (boxed: it is large, the others are not).
    Update(Box<Update>),
    /// Whether a daemon serves the profile changed.
    /// `None` when the lock cannot tell.
    Daemon(Option<bool>),
    /// The store could not be listed at start: what it holds stays
    /// unshown this session. A class, never content.
    ListingFailed,
    /// The facade could not be built: its store's pending rows could not
    /// be read. Nothing is sent or received this session.
    FacadeFailed,
    /// The session is closed and the thread is ending.
    Closed,
}

/// The window's handle on the facade thread.
pub struct FacadeThread {
    // Unbounded on purpose: only the person's own actions fill it, at
    // most one take of the view's bounded queue per turn of the window,
    // and the facade drains it at every turn of its own.
    to_facade: UnboundedSender<ToFacade>,
    from_facade: mpsc::Receiver<FromFacade>,
    /// A wake is asked of the window and not yet answered by a take: one
    /// at a time, however many messages wait.
    woken: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl FacadeThread {
    /// Start the facade side `make` builds, on a thread of its own (`make`
    /// fails only when the store's pending rows cannot be read).
    /// `daemon_present` asks whether a daemon serves the profile (`Err`
    /// when it cannot tell: that is logged once per change, never read as
    /// "absent").
    /// `notify` is called, from the facade thread, when there is something
    /// to take and no wake is outstanding: the window schedules a turn
    /// with it, and its next [`take`](Self::take) answers it. `report`
    /// takes the thread's log lines (stderr, in the app).
    ///
    /// # Errors
    /// When the thread or its runtime cannot be started.
    pub fn spawn<B, A>(
        make: impl FnOnce() -> Result<FacadeSide<B, A>, StoreError> + Send + 'static,
        daemon_present: impl Fn() -> Result<bool, String> + Send + 'static,
        notify: Arc<dyn Fn() + Send + Sync>,
        report: fn(&str),
    ) -> std::io::Result<Self>
    where
        B: DataSessionBinding + 'static,
        A: AdminBinding + 'static,
    {
        Self::spawn_bounded(make, daemon_present, notify, report, TO_WINDOW)
    }

    /// [`spawn`](Self::spawn), with at most `capacity` messages waiting
    /// for the window instead of [`TO_WINDOW`].
    ///
    /// # Errors
    /// When the thread or its runtime cannot be started.
    pub fn spawn_bounded<B, A>(
        make: impl FnOnce() -> Result<FacadeSide<B, A>, StoreError> + Send + 'static,
        daemon_present: impl Fn() -> Result<bool, String> + Send + 'static,
        notify: Arc<dyn Fn() + Send + Sync>,
        report: fn(&str),
        capacity: usize,
    ) -> std::io::Result<Self>
    where
        B: DataSessionBinding + 'static,
        A: AdminBinding + 'static,
    {
        let (to_facade, mut commands) = unbounded_channel();
        let (send, from_facade) = mpsc::sync_channel(capacity);
        let woken = Arc::new(AtomicBool::new(false));
        let wake = Arc::clone(&woken);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()?;
        let thread = std::thread::Builder::new()
            .name("human-facade".to_owned())
            .spawn(move || {
                let start = Instant::now();
                let now = || u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
                let out = |message: FromFacade| {
                    // Waits while the window holds `capacity` untaken: the
                    // backpressure the bound is for. A window that closes
                    // drains it; one that is gone drops the receiver, and
                    // the send fails instead -- nothing to tell then.
                    if send.send(message).is_err() {
                        return;
                    }
                    // Sent before the flag is read, and the take clears
                    // the flag before it reads: a message is either in
                    // that take or asks a wake of its own.
                    if !wake.swap(true, Ordering::SeqCst) {
                        notify();
                    }
                };
                runtime.block_on(async move {
                    let Ok(mut side) = make() else {
                        out(FromFacade::FacadeFailed);
                        return;
                    };
                    match side.listing() {
                        Ok(listing) => out(FromFacade::Update(Box::new(Update::Listed(listing)))),
                        Err(_) => out(FromFacade::ListingFailed),
                    }
                    let mut daemon: Option<Option<bool>> = None;
                    let mut cannot_tell: Option<String> = None;
                    let mut probed: Option<Instant> = None;
                    loop {
                        let message = tokio::select! {
                            message = commands.recv() => Some(message),
                            () = tokio::time::sleep(POLL) => None,
                        };
                        match message {
                            Some(Some(ToFacade::Command(command))) => {
                                for update in side.execute(command, now()).await {
                                    out(FromFacade::Update(Box::new(update)));
                                }
                            }
                            Some(Some(ToFacade::Close) | None) => {
                                side.close().await;
                                break;
                            }
                            None => {}
                        }
                        for update in side.turn(now()).await {
                            out(FromFacade::Update(Box::new(update)));
                        }
                        if probed.is_none_or(|at| at.elapsed() >= DAEMON_PROBE) {
                            probed = Some(Instant::now());
                            let answer = daemon_present();
                            let seen = answer.as_ref().ok().copied();
                            // The window hears every change, cannot-tell
                            // included: an earlier "no daemon" must not
                            // outlive a lock that can no longer answer.
                            if daemon != Some(seen) {
                                daemon = Some(seen);
                                out(FromFacade::Daemon(seen));
                            }
                            match answer {
                                Ok(_) => cannot_tell = None,
                                // Said once, and again only when the reason
                                // changes.
                                Err(why) => {
                                    if cannot_tell.as_ref() != Some(&why) {
                                        report(&format!(
                                            "whether the transport daemon runs cannot be told: \
                                             {why}"
                                        ));
                                        cannot_tell = Some(why);
                                    }
                                }
                            }
                        }
                    }
                });
                out(FromFacade::Closed);
            })?;
        Ok(Self {
            to_facade,
            from_facade,
            woken,
            thread: Some(thread),
        })
    }

    /// Ask the facade side to carry out `command`. False once the facade
    /// thread has ended.
    #[must_use]
    pub fn command(&self, command: Command) -> bool {
        self.to_facade.send(ToFacade::Command(command)).is_ok()
    }

    /// What the facade side sent since the last call, in order. Answers
    /// the outstanding wake, so the next message asks for another.
    #[must_use]
    pub fn take(&self) -> Vec<FromFacade> {
        self.woken.store(false, Ordering::SeqCst);
        self.from_facade.try_iter().collect()
    }

    /// Close the session and wait for the thread, at most `wait`. The
    /// lease is released; the daemon keeps running. True when the thread
    /// ended in time.
    #[must_use]
    pub fn close(mut self, wait: Duration) -> bool {
        let _ = self.to_facade.send(ToFacade::Close);
        let deadline = Instant::now() + wait;
        while Instant::now() < deadline {
            match self.from_facade.recv_timeout(Duration::from_millis(20)) {
                // Closed now, or ended already: a thread that ended before
                // the window asked -- its facade could not be built, its
                // Closed taken by a pump -- has dropped its sender.
                Ok(FromFacade::Closed) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    if let Some(thread) = self.thread.take() {
                        let _ = thread.join();
                    }
                    return true;
                }
                Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
        // Past the deadline the process exits anyway: the OS closes the
        // socket, and the daemon releases the lease on its own.
        false
    }
}
