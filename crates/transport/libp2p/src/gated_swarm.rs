// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The Swarm, with dialing reachable only through admission.
//!
//! # Why a wrapper and not a rule
//!
//! ADR-0011's exit gate says root admission is **the only** policy
//! authority for outbound Swarm dials. Written as a convention — "call
//! `admit` before `dial`" — that lasts exactly until someone adds a
//! second call site. This session has already watched the same shape
//! fail three times over: the /64 bucketing rule published as advice
//! nothing applied, a `source_bucket` helper documented as essential and
//! called by nothing, and then a bucket function that did not recognise
//! the string its only real caller holds.
//!
//! So the raw `Swarm` is private to this type and [`Self::dial`] takes
//! an [`AdmittedDial`], which cannot be built without a
//! [`DialTicket`], which only [`PolicySnapshot::admit`] issues. A call
//! site that forgets to ask does not misbehave at runtime — it does not
//! compile.
//!
//! # What this does NOT cover
//!
//! Dials that a `NetworkBehaviour` originates from inside the Swarm.
//! Those never pass through this API at all; libp2p routes them through
//! `NetworkBehaviour::handle_pending_outbound_connection`, and that hook
//! is where the same ticket has to be required. Stage 4's behaviour set
//! is TCP, Noise, Yamux and Identify — none of which dials — so there is
//! nothing to gate there yet, and the honest statement is that this
//! closes the command path and the behaviour path is closed when the
//! first dialing behaviour arrives. Kademlia must not be enabled before
//! it is.
//!
//! [`PolicySnapshot::admit`]: interweave_transport_runtime::PolicySnapshot::admit

use futures::stream::SelectNextSome;
use libp2p::core::transport::ListenerId;
use libp2p::swarm::dial_opts::DialOpts;
use libp2p::swarm::{ConnectionId, DialError, Swarm, SwarmEvent};
use libp2p::{Multiaddr, PeerId, TransportError};

use interweave_transport_runtime::DialTicket;

use crate::behaviour::{SubstrateBehaviour, SubstrateBehaviourEvent};

/// A dial that has passed admission, carrying the proof.
///
/// Constructible only from a [`DialTicket`]. There is no other way to
/// reach [`GatedSwarm::dial`], and no way to obtain a ticket except by
/// being admitted.
#[derive(Debug)]
#[must_use = "an admitted dial holds a pending-dial slot until it is executed or dropped"]
pub struct AdmittedDial {
    opts: DialOpts,
    ticket: DialTicket,
}

impl AdmittedDial {
    /// Pair an admission with the dial it authorizes.
    pub fn new(ticket: DialTicket, opts: DialOpts) -> Self {
        Self { opts, ticket }
    }

    /// The connection this dial will use, known before it is made.
    ///
    /// The key the ticket is filed under, so the outcome event can find
    /// the admission it belongs to. Reading it here rather than after
    /// dialling matters: on a synchronous failure there is no event, and
    /// a ticket filed under an id nothing will ever report is a leaked
    /// slot.
    #[must_use]
    pub fn connection_id(&self) -> ConnectionId {
        self.opts.connection_id()
    }
}

/// The Swarm, with `dial` reachable only through [`AdmittedDial`].
///
/// Forwards exactly the operations the runtime uses. Deliberately not
/// `Deref`: dereferencing to the inner `Swarm` would hand back the
/// ungated `dial` and undo the whole point.
pub struct GatedSwarm {
    inner: Swarm<SubstrateBehaviour>,
}

impl core::fmt::Debug for GatedSwarm {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // `Swarm` is not `Debug`. The local identity is the only thing
        // worth printing and the only thing that is stable.
        f.debug_struct("GatedSwarm")
            .field("local_peer_id", self.inner.local_peer_id())
            .finish_non_exhaustive()
    }
}

impl GatedSwarm {
    /// Wrap a built Swarm.
    pub const fn new(inner: Swarm<SubstrateBehaviour>) -> Self {
        Self { inner }
    }

    /// The local identity.
    #[must_use]
    pub fn local_peer_id(&self) -> &PeerId {
        self.inner.local_peer_id()
    }

    /// Begin listening.
    ///
    /// # Errors
    /// Whatever the transport reports.
    pub fn listen_on(
        &mut self,
        address: Multiaddr,
    ) -> Result<ListenerId, TransportError<std::io::Error>> {
        self.inner.listen_on(address)
    }

    /// Stop a listener.
    pub fn remove_listener(&mut self, id: ListenerId) -> bool {
        self.inner.remove_listener(id)
    }

    /// The next Swarm event.
    pub fn select_next_some(&mut self) -> SelectNextSome<'_, Swarm<SubstrateBehaviour>>
    where
        Swarm<SubstrateBehaviour>:
            futures::Stream<Item = SwarmEvent<SubstrateBehaviourEvent>> + Unpin,
    {
        futures::StreamExt::select_next_some(&mut self.inner)
    }

    /// Dial, given an admission.
    ///
    /// Returns the ticket back on a SYNCHRONOUS failure. libp2p reports
    /// no event for a dial it refused outright, so a caller that filed
    /// the ticket and walked away would hold a pending-dial slot until
    /// the process ended — the resource bound decaying every time a
    /// malformed address is tried.
    ///
    /// # Errors
    /// The dial error, paired with the unspent admission.
    pub fn dial(
        &mut self,
        admitted: AdmittedDial,
    ) -> Result<DialTicket, Box<(DialError, DialTicket)>> {
        let AdmittedDial { opts, ticket } = admitted;
        match self.inner.dial(opts) {
            Ok(()) => Ok(ticket),
            // Boxed: `DialError` is large, and an unboxed error variant
            // would make every successful dial carry its size.
            Err(e) => Err(Box::new((e, ticket))),
        }
    }
}
