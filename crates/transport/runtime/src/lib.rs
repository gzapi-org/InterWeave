// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Backend-neutral transport runtime state machines.
//!
//! Stage 2 activates the PURE modules only: decisions and state that can
//! be tested by enumeration rather than orchestration. Nothing here opens
//! a socket, starts a Swarm, or reads a clock.

#![forbid(unsafe_code)]

pub mod endpoint_registry;

pub use endpoint_registry::{
    ActiveLease, ClaimFailure, EndpointRegistry, LocalSessionId, RegisteredEndpoint, ResolveFailure,
};
