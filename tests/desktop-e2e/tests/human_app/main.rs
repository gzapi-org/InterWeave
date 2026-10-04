// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The desktop human client's required end-to-end cases (plan section 18,
//! "Required desktop E2E"): one named test per bullet, each against real
//! daemons started by `common/`'s harness, the client the shipped
//! `human-desktop` binary on a real display.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic)]

// The daemon's harness, p2p-network-dev's; shared with the suite's other
// targets.
#[path = "../common/mod.rs"]
mod common;

mod daemon_link;
mod endpoints;
mod harness;
mod lifecycle;
mod retention;
mod storage;
mod world;
