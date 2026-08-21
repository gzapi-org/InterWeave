// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The authenticated transport substrate.
//!
//! Stage 4 of the canonical plan: TCP, Noise, Yamux, Identify, and
//! nothing else. Two peers can listen, dial, authenticate each other's
//! PeerId, exchange Identify, and shut down. No application protocol
//! runs over it yet.
//!
//! # What is absent, and why it is absent rather than merely unused
//!
//! GossipSub, direct v2, Kademlia, AutoNAT, Circuit Relay and DCUtR are
//! not in the `libp2p` feature list this crate compiles against. They
//! cannot be switched on by a `use` statement or a stray builder call,
//! because the code is not there. CLAUDE.md §3 forbids enabling
//! autonomous libp2p behaviour and retrofitting admission policy
//! afterwards, and the cheapest way to keep that promise is to not
//! compile the behaviour.
//!
//! # Nothing above this crate sees a libp2p type
//!
//! The boundary speaks [`interweave_transport_api::TransportIdentity`]
//! and this crate's own [`SwarmCommand`] and [`SwarmEvent`]. That keeps
//! the backend replaceable and keeps `crates/api/*` free of libp2p
//! (CLAUDE.md §4) — the translation happens once, here.
//!
//! # Dials are admitted, from the first line of substrate code
//!
//! [`SwarmRuntime`] runs every dial through
//! [`interweave_transport_runtime::ConnectionPolicy`], which Stage 2
//! implemented and tested. Stage 5 owns making that gate *root* — every
//! behaviour-originated dial, the ConnectionManager, the address
//! scheduler. What Stage 4 refuses to do is ship a dial path with no
//! gate at all and add one later, which is the retrofit CLAUDE.md warns
//! against.

#![forbid(unsafe_code)]

pub mod behaviour;
pub mod runtime;

pub use behaviour::{IDENTIFY_PROTOCOL, SubstrateBehaviour};
pub use runtime::{
    DEFAULT_COMMAND_CAPACITY, DEFAULT_EVENT_CAPACITY, DialRefusal, MAX_CONFIGURED_CAPACITY,
    SubstrateConfig, SubstrateError, SwarmCommand, SwarmEvent, SwarmRuntime,
};
