// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The Android human client's platform side (plan section 20 steps 2-4):
//! what the foreground Service holds -- the embedded transport runtime,
//! the message store and the facade over the runtime's in-process binding
//! ([`ServiceHost`]) -- and where the Activity's view and the platform's
//! notifications meet it ([`Hub`]).
//!
//! No Android, JNI or Slint type: the app's native library carries the
//! JNI entry points and the views, so everything here builds and is
//! tested on the host.

#![forbid(unsafe_code)]

pub mod facade;
pub mod hub;
pub mod notice;
pub mod service;
#[cfg(feature = "dev-stand-ins")]
pub mod stand_in;

pub use hub::{Hub, TO_VIEW, ToView, ViewLink};
pub use notice::Notices;
pub use service::{
    AvailabilityMode, CLIENT_KIND, Ended, ServiceHost, ServiceLaunch, StartRefused, Stopped,
};
