// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! TEST BUILDS ONLY (feature `e2e-cases`): the app's own client as an
//! android-e2e case drives it (`interweave-android-e2e-cases`'
//! `AppClient`) -- a view attached to the service's hub, as the window
//! attaches, so a case sends and reads through what the window does.
//! The instrumentation on the phone and the host's stand-in for it both
//! use this, so the two runners hand a case the same thing.

use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

use interweave_android_e2e_cases::{AppClient, Handed, Tap};
use interweave_human_app_core::{Command, Update};

use crate::hub::{Hub, ToView, ViewLink};

/// A view on the hub, for a case.
pub struct HubClient {
    hub: Arc<Hub>,
    link: ViewLink,
    woken: Arc<(Mutex<bool>, Condvar)>,
    tap: Tap,
    /// The service was seen running: a later `Running(false)` is its end.
    running: bool,
}

impl HubClient {
    /// Attach to `hub`, as the window does, reading what crossed from
    /// `tap` (the one [`crate::ServiceHost::start_recording`] was given).
    /// It takes the hub's one view slot: an attached window is detached.
    #[must_use]
    pub fn attach(hub: &Arc<Hub>, tap: Tap) -> Self {
        let woken = Arc::new((Mutex::new(false), Condvar::new()));
        let notify = {
            let woken = Arc::clone(&woken);
            Arc::new(move || {
                let (flag, ready) = &*woken;
                *flag.lock().unwrap_or_else(PoisonError::into_inner) = true;
                ready.notify_all();
            })
        };
        Self {
            hub: Arc::clone(hub),
            link: hub.attach(notify),
            woken,
            tap,
            running: false,
        }
    }
}

impl AppClient for HubClient {
    fn command(&mut self, command: Command) -> bool {
        self.hub.command(command)
    }

    fn updates(&mut self, wait: Duration) -> Result<Vec<Update>, String> {
        {
            let (flag, ready) = &*self.woken;
            let woken = flag.lock().unwrap_or_else(PoisonError::into_inner);
            let (mut woken, _) = ready
                .wait_timeout_while(woken, wait, |woken| !*woken)
                .unwrap_or_else(PoisonError::into_inner);
            *woken = false;
        }
        let mut updates = Vec::new();
        for message in self.link.take() {
            match message {
                ToView::Running(true) => self.running = true,
                ToView::Running(false) if self.running => {
                    return Err("the app's service stopped".to_owned());
                }
                ToView::Running(false) => {}
                ToView::Update(update) => updates.push(*update),
                ToView::ListingFailed => {
                    return Err("the store could not be listed for the view".to_owned());
                }
            }
        }
        if self.link.lost() {
            return Err("the hub detached the view".to_owned());
        }
        Ok(updates)
    }

    fn handed(&self) -> Vec<Handed> {
        self.tap
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}
