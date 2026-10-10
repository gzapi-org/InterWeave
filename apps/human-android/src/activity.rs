// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The Activity's side: the views on the platform's `NativeActivity`,
//! attached to the network service's session through the hub. The
//! session is the Service's: an Activity destroyed or rotated detaches,
//! and the next one attaches and lists the store again.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use interweave_human_android_platform::{ServiceHost, ViewLink, ViewSide};
use interweave_human_app_core::{ModelSide, Opener, Problem, Surface};
use interweave_human_ui_model::{RuntimeHost, UiModel, ViewEvent};
use interweave_human_ui_slint::{
    ActivityEvent, AndroidApp, View, defer, init_android, invoke_on_window,
};

use crate::jni::log;

/// The view, shared by every model side the Activity makes: one per
/// attach, since a service that starts again lists its store from new.
struct AndroidSurface(Rc<RefCell<View>>);

impl Surface for AndroidSurface {
    fn render(&mut self, model: &UiModel) {
        self.0.borrow_mut().render(model);
    }

    fn take_events(&mut self, model: &UiModel) -> Vec<ViewEvent> {
        self.0.borrow_mut().take_events(model)
    }
}

/// A link's destination. Not opened yet: the platform's intent is a
/// later step, and a link pressed now is logged, never followed.
struct AndroidOpener;

impl Opener for AndroidOpener {
    fn open(&mut self, _destination: &str) {
        log("a link was pressed: opening links is not built yet on Android");
    }
}

struct Root {
    view: Rc<RefCell<View>>,
    side: ViewSide<AndroidSurface, AndroidOpener>,
    link: ViewLink,
}

impl Root {
    fn attach(view: Rc<RefCell<View>>) -> Self {
        let link = ServiceHost::global().hub().attach(Arc::new(|| {
            invoke_on_window(turn);
        }));
        let surface = Rc::clone(&view);
        Self {
            side: ViewSide::new(move || {
                ModelSide::new(AndroidSurface(Rc::clone(&surface)), AndroidOpener)
            }),
            view,
            link,
        }
    }

    /// One turn: what the hub sent, applied; the person's commands, to
    /// the facade.
    fn pump(&mut self) {
        let hub = ServiceHost::global().hub();
        for message in self.link.take() {
            // A change of the service's running state starts a fresh model
            // either way (android-platform's ViewSide says why).
            if self.side.apply(message).is_err() {
                log("the message store could not be listed");
            }
        }
        if self.link.lost() {
            // Detached by the hub: attach again, and start from a listing.
            let view = Rc::clone(&self.view);
            *self = Self::attach(view);
            defer(turn);
            return;
        }
        for command in self.side.side_mut().turn() {
            if !hub.command(command) {
                log("a command found no running network service");
            }
        }
        for problem in self.side.side_mut().take_problems() {
            match problem {
                Problem::Command { why, .. } => log(&format!("a command failed: {why:?}")),
                Problem::UnreadNotListed(why) => {
                    log(&format!("unread messages could not be listed: {why:?}"));
                }
            }
        }
    }
}

thread_local! {
    // The window side, on the Activity's thread only; the facade thread
    // reaches it through `invoke_on_window`, which runs on this thread.
    static ROOT: RefCell<Option<Root>> = const { RefCell::new(None) };
}

/// One turn of the window side, if it is there and not already turning.
fn turn() {
    ROOT.with(|root| {
        if let Ok(mut root) = root.try_borrow_mut() {
            if let Some(root) = root.as_mut() {
                root.pump();
            }
        } else {
            defer(turn);
        }
    });
}

/// The window gained or lost the person's focus: reads follow it, and
/// so do the notices (a focused window reads what arrives).
fn focus(focused: bool) {
    ServiceHost::global().hub().set_focused(focused);
    let applied = ROOT.with(|root| {
        let Ok(root) = root.try_borrow() else {
            return false;
        };
        if let Some(root) = root.as_ref() {
            root.view.borrow_mut().set_window_focused(focused);
        }
        true
    });
    if applied {
        defer(turn);
    } else {
        defer(move || focus(focused));
    }
}

/// Called by the `NativeActivity` glue on a thread of its own, once per
/// Activity instance.
#[allow(
    unsafe_code,
    clippy::no_mangle_with_rust_abi,
    reason = "android-activity's glue calls android_main by its unmangled name with the Rust ABI"
)]
#[unsafe(no_mangle)]
fn android_main(app: AndroidApp) {
    log("android_main: the Activity's views start");
    if let Err(e) = init_android(app, |event| match event {
        ActivityEvent::Focus(focused) => focus(focused),
        ActivityEvent::Destroy => ServiceHost::global().hub().set_focused(false),
    }) {
        log(&format!("the views' platform could not be set: {e}"));
        return;
    }
    let view = match View::new(RuntimeHost::Embedded) {
        Ok(view) => view,
        Err(e) => {
            log(&format!("no window: {e}"));
            return;
        }
    };
    let handle = view.handle();
    view.set_wake(|| defer(turn));
    let view = Rc::new(RefCell::new(view));
    ROOT.with(|root| *root.borrow_mut() = Some(Root::attach(view)));
    defer(turn);
    if let Err(e) = handle.run() {
        log(&format!("the event loop ended: {e}"));
    }
    // The Activity is gone: its link goes with it, and the hub detaches
    // it on its next send. The session stays the Service's.
    ROOT.with(|root| root.borrow_mut().take());
    log("android_main: the Activity's views end");
}
