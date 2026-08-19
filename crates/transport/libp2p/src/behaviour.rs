// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The Stage 4 network behaviour: Identify, and nothing else.
//!
//! One behaviour, deliberately. Every additional protocol here is a
//! protocol that starts doing things on its own — Kademlia dials to fill
//! buckets, AutoNAT probes, Relay renews reservations — and each of
//! those is an outbound dial that must already be passing the root
//! admission gate before it exists (CLAUDE.md §3). Identify does not
//! originate dials; it answers on connections that already exist, which
//! is why it is the one that can be here now.

// The `NetworkBehaviour` derive generates `SubstrateBehaviourEvent` as a
// sibling item, and its variants carry no documentation the derive could
// have written. The allowance is scoped to THIS module — which holds
// nothing but the behaviour and its constructor, both documented — rather
// than to the crate, so every hand-written type elsewhere still has to
// document itself.
#![allow(missing_docs, reason = "variants of the derive-generated event enum")]

use libp2p::swarm::NetworkBehaviour;
use libp2p::{identify, identity};

/// The Identify protocol name this profile advertises.
///
/// Namespaced under `interweave` per ADR-0047, and versioned so a future
/// change is a new string rather than a silent reinterpretation.
pub const IDENTIFY_PROTOCOL: &str = "/interweave/id/1.0.0";

/// The Stage 4 behaviour.
#[derive(NetworkBehaviour)]
pub struct SubstrateBehaviour {
    /// Peer metadata exchange on an already-established connection.
    pub identify: identify::Behaviour,
}

impl SubstrateBehaviour {
    /// Build the behaviour for `public_key`.
    #[must_use]
    pub fn new(public_key: identity::PublicKey) -> Self {
        Self {
            identify: identify::Behaviour::new(identify::Config::new(
                IDENTIFY_PROTOCOL.to_owned(),
                public_key,
            )),
        }
    }
}
