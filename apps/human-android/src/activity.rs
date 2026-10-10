// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The Activity's side: the views on the platform's NativeActivity.

use interweave_human_ui_model::{RuntimeHost, UiModel};
use interweave_human_ui_slint::{ActivityEvent, AndroidApp, View, init_android};

/// Called by the NativeActivity glue on a thread of its own, once per
/// Activity instance.
#[allow(
    unsafe_code,
    reason = "the NativeActivity glue finds android_main by its unmangled name"
)]
#[unsafe(no_mangle)]
fn android_main(app: AndroidApp) {
    if let Err(e) = init_android(app, |event: ActivityEvent| eprintln!("activity: {event:?}")) {
        eprintln!("human-android: the views' platform could not be set: {e}");
        return;
    }
    let mut view = match View::new(RuntimeHost::Embedded) {
        Ok(view) => view,
        Err(e) => {
            eprintln!("human-android: no window: {e}");
            return;
        }
    };
    view.render(&UiModel::new());
    if let Err(e) = view.handle().run() {
        eprintln!("human-android: the event loop ended: {e}");
    }
}
