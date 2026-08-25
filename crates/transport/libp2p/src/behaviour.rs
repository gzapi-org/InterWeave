// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The network behaviour: pre-auth admission and Identify.
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

use std::time::Duration;

use libp2p::request_response::{self, ProtocolSupport};
use libp2p::swarm::NetworkBehaviour;
use libp2p::{identify, identity};

use interweave_transport_runtime::preauth::PreAuthLimits;

use crate::direct_codec::{DIRECT_PROTOCOL, DirectCodec};
use crate::outbound_gate::OutboundAdmission;
use crate::preauth_gate::PreAuthAdmission;

/// The total deadline for one direct exchange (`DIRECT.md`).
///
/// Ten seconds, and it is the REQUESTER's patience rather than a promise
/// about the responder: SPIKE-002 finding 1 showed that when both sides
/// time out the attribution is a race, so this bounds how long a caller
/// waits and nothing more.
pub const DIRECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The Identify protocol name this profile advertises.
///
/// Namespaced under `interweave` per ADR-0047, and versioned so a future
/// change is a new string rather than a silent reinterpretation.
pub const IDENTIFY_PROTOCOL: &str = "/interweave/id/1.0.0";

/// The Stage 4 behaviour, plus the gate that decides who may begin.
#[derive(NetworkBehaviour)]
pub struct SubstrateBehaviour {
    /// Pre-Noise admission for inbound connections.
    ///
    /// FIRST, and the order is not cosmetic: the derive calls each
    /// field's `handle_pending_inbound_connection` in declaration
    /// order and stops at the first `Err`, so a denial here costs
    /// nothing further. It is also the field that must exist before
    /// any behaviour that dials, which is why it lands with Stage 5
    /// rather than with the first behaviour that needs it.
    pub preauth: PreAuthAdmission,
    /// The gate every outbound dial passes, including a behaviour's.
    ///
    /// Present before any behaviour that dials exists, which is the
    /// order CLAUDE.md §3 requires: the funnel is green first, and
    /// Kademlia is added to a Swarm that already refuses an
    /// unadmitted dial.
    pub outbound: OutboundAdmission,
    /// Peer metadata exchange on an already-established connection.
    pub identify: identify::Behaviour,
    /// Directed messaging, `/interweave/direct/2.0.0`.
    ///
    /// LAST, and after both gates, because the derive calls each field's
    /// handlers in declaration order. This behaviour originates outbound
    /// dials when a caller sends to a peer it is not connected to, so it
    /// is added to a Swarm where `outbound` already refuses an unadmitted
    /// dial and `preauth` already answers before Noise — the ordering
    /// CLAUDE.md §3 requires, and the reason Stage 5 had to be green
    /// before this field could exist at all.
    pub direct: request_response::Behaviour<DirectCodec>,
}

impl SubstrateBehaviour {
    /// Build the behaviour for `public_key`.
    #[must_use]
    pub fn new(
        public_key: identity::PublicKey,
        preauth: PreAuthLimits,
        max_payload_bytes: usize,
    ) -> Self {
        Self {
            preauth: PreAuthAdmission::new(preauth),
            outbound: OutboundAdmission::default(),
            identify: identify::Behaviour::new(identify::Config::new(
                IDENTIFY_PROTOCOL.to_owned(),
                public_key,
            )),
            direct: request_response::Behaviour::with_codec(
                DirectCodec::new(max_payload_bytes),
                // FULL, because a profile both sends and receives directed
                // messages. Inbound-only would make this peer unable to
                // initiate, which is not a security posture — an
                // unauthorized peer is refused by trust, not by declining
                // to speak.
                [(DIRECT_PROTOCOL, ProtocolSupport::Full)],
                request_response::Config::default().with_request_timeout(DIRECT_TIMEOUT),
            ),
        }
    }
}
