// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The desktop human client's required end-to-end cases (plan section 18,
//! "Required desktop E2E"): one named test per bullet, each against real
//! daemons started by `common/`'s harness, the client the shipped
//! `human-desktop` binary on a real display. Where a case needs a person
//! -- to read, keep, or hear the window -- it reads and presses the
//! window over AT-SPI as a screen reader does (`a11y.rs`), the display's
//! focus given as a window manager would (`display.rs`).

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic)]

// The daemon's harness, p2p-network-dev's; shared with the suite's other
// targets.
#[path = "../common/mod.rs"]
mod common;

mod a11y;
mod accessibility;
mod chat;
mod daemon_link;
mod display;
mod endpoints;
mod harness;
mod lifecycle;
mod reading;
mod retention;
mod storage;
mod trust;
mod world;
