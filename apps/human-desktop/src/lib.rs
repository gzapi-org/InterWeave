// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The desktop human client's composition root (plan section 18). Thin by
//! rule: the root's logic is `crates/human/app-core`; this crate holds the
//! command line, the profile's paths and store, the facade's thread and
//! runtime over the IPC binding, and the window's side.

#![forbid(unsafe_code)]

#[cfg(unix)]
pub mod app;
#[cfg(unix)]
pub mod daemon;
#[cfg(unix)]
pub mod facade_thread;
#[cfg(unix)]
pub mod ipc;
#[cfg(unix)]
pub mod run;
#[cfg(unix)]
mod signals;
#[cfg(unix)]
pub mod startup;
#[cfg(unix)]
mod window;
