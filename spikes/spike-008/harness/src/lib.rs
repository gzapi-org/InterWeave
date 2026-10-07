// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! SPIKE-008's Rust core: the production human store, driven the way
//! `RETENTION.md` says a client drives it, so the device run measures
//! what the store keeps across a kill and a reboot, and what its backup
//! predicate offers, on Android's filesystem.
//!
//! Every row's payload is a TEST label (`P1`, `U2`, `K3`, ...), so a
//! census names rows without any message content.

pub mod store;

mod jni;
