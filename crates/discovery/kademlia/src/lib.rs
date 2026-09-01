// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! [`KademliaDiscovery`]: the DHT as a `DiscoveryProvider`.
//!
//! This crate is the provider half of the split that
//! `kademlia-integration.md` §20 draws: everything libp2p-shaped lives in
//! the Swarm-owned driver, and everything project-shaped — targeted-lookup
//! eligibility (§9.2), result normalization (§10), health (§14) — lives
//! here, over the bounded command/event port of `kademlia-control-api`.
//! The composition root pumps both directions.
//!
//! Kademlia is **peer routing only** (ADR-0009). A candidate this provider
//! emits is an advisory reachability observation with `"kademlia"`
//! provenance and a TTL; it is never trust, membership, or authorization.

#![forbid(unsafe_code)]

mod budgets;
mod health;
mod normalize;
mod provider;
mod scheduler;

pub use provider::{
    BootstrapRefusal, KademliaDiscovery, KademliaProviderConfig, MAX_CAPABILITY_EVIDENCE,
    MAX_IMPLICIT_CHARGES, MAX_PENDING_COMMANDS, MAX_TRACKED_CANDIDATES, ProviderConfigError,
    SOURCE, TargetedRefusal,
};
