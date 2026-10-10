// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The window's event loop: the root's window side lives on this thread,
//! and everything that wants a turn -- the view's own input, the facade
//! thread's updates, the window's focus -- schedules one here.

use std::cell::RefCell;
use std::sync::Arc;
use std::time::Duration;

use interweave_human_store::HumanStore;
use interweave_human_ui_model::RuntimeHost;
use interweave_human_ui_slint::{View, defer, invoke_on_window, quit_event_loop};

use crate::app::{App, DesktopOpener, SlintSurface};
use crate::facade_thread::FacadeThread;
use crate::ipc::facade_over_ipc;
use crate::startup::Profile;
use crate::{daemon, signals};

/// How long closing waits for the facade thread to release the lease.
const CLOSE_WAIT: Duration = Duration::from_secs(5);

type DesktopApp = App<SlintSurface, DesktopOpener>;

thread_local! {
    // The window side, reachable from the event loop's callbacks. Only the
    // window's thread touches it; the facade thread reaches it through
    // `invoke_on_window`, which runs on this thread.
    static ROOT: RefCell<Option<DesktopApp>> = const { RefCell::new(None) };
}

/// One turn of the window side, if it is there and not already turning: a
/// turn asked for while one runs is deferred to run after it.
fn turn() {
    ROOT.with(|root| {
        if let Ok(mut root) = root.try_borrow_mut() {
            if let Some(app) = root.as_mut() {
                app.pump();
            }
        } else {
            defer(turn);
        }
    });
}

/// The window gained or lost focus: reads follow it. Arriving during a
/// turn, it is deferred like a turn rather than dropped -- a lost
/// `false` would leave the view marking messages read while the person
/// is elsewhere.
fn focus(focused: bool) {
    let applied = ROOT.with(|root| {
        let Ok(mut root) = root.try_borrow_mut() else {
            return false;
        };
        if let Some(app) = root.as_mut() {
            app.side_mut().surface_mut().0.set_window_focused(focused);
        }
        true
    });
    if applied {
        defer(turn);
    } else {
        defer(move || focus(focused));
    }
}

/// Open the window for `profile` over `store`, run until it is closed or
/// a signal ends it, then close the session: the endpoint lease is
/// released, and the daemon keeps running (ADR-0040).
///
/// # Errors
/// The toolkit's, as text: no window could be created or shown.
pub(crate) fn run(profile: &Profile, store: HumanStore) -> Result<(), String> {
    let view = View::new(RuntimeHost::Daemon).map_err(|e| e.to_string())?;
    let handle = view.handle();
    view.set_wake(|| defer(turn));
    view.on_window_focus(focus);

    // Before the facade asks for the lease: from then on a signal must
    // close the session, not kill the process holding it.
    signals::on_terminate(|| {
        invoke_on_window(quit_event_loop);
    });
    let paths = profile.paths.clone();
    let facade = FacadeThread::spawn(
        facade_over_ipc(profile, store),
        move || daemon::present(&paths),
        Arc::new(|| {
            invoke_on_window(turn);
        }),
        |line| eprintln!("human-desktop: {line}"),
    )
    .map_err(|e| e.to_string())?;
    ROOT.with(|root| {
        *root.borrow_mut() = Some(App::new(SlintSurface(view), DesktopOpener::new(), facade));
    });
    defer(turn);

    let ran = handle.run();
    let app = ROOT.with(|root| root.borrow_mut().take());
    if let Some(mut app) = app
        && !app.close(CLOSE_WAIT)
    {
        eprintln!("human-desktop: the transport side did not close in time");
    }
    ran
}
