// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The other half of the dial gate: the one behaviours cannot walk past.
//!
//! # Two doors, and this is the one nobody knocks on yet
//!
//! [`GatedSwarm`](crate::gated_swarm::GatedSwarm) closes the command
//! path: the raw `Swarm` is private, and `dial` needs an admission
//! ticket. That says nothing about dials a `NetworkBehaviour`
//! originates from inside the Swarm — Kademlia filling a bucket,
//! AutoNAT probing, Relay renewing a reservation. Those never pass
//! through the wrapper at all.
//!
//! libp2p routes every dial, whatever asked for it, through
//! `NetworkBehaviour::handle_pending_outbound_connection`, and it does
//! so synchronously inside `Swarm::dial`. So this behaviour keeps the
//! set of connection ids the root admission has just issued a ticket
//! for, and denies any dial whose id is not in it. A behaviour-
//! originated dial has an id no ticket was issued for, so it is
//! refused.
//!
//! # Why it lands before anything that dials
//!
//! CLAUDE.md §3: root admission must be implemented and green BEFORE
//! Kademlia, AutoNAT, Relay or DCUtR are activated. Writing the hook
//! afterwards is the retrofit that rule exists to forbid, and "no
//! behaviour dials yet" is exactly the moment when adding it costs
//! nothing and proves something.
//!
//! # What it is not
//!
//! Not a policy. The decision was made by the root
//! [`PolicySnapshot::admit`](interweave_transport_runtime::PolicySnapshot::admit);
//! this only enforces that a decision was made at all, for THIS dial.
//! The ticket itself, and the peer and address bound into it, are
//! checked where they are built.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use libp2p::PeerId;
use libp2p::core::transport::PortUse;
use libp2p::core::{Endpoint, Multiaddr};
use libp2p::swarm::{
    ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, THandler, THandlerInEvent,
    THandlerOutEvent, ToSwarm, dummy,
};

/// What a refused behaviour dial is told.
const REFUSAL: &str = "outbound connections require a root dial admission";

/// Connection ids the root admission has issued a ticket for.
///
/// Shared between the wrapper that dials and the behaviour that
/// answers, because the two are the same decision seen from either end
/// of one synchronous call: `Swarm::dial` invokes the hook before it
/// returns, so an id is registered and consumed within a single
/// statement and the set is empty in between.
#[derive(Debug, Clone, Default)]
pub struct AdmittedDials {
    ids: Arc<Mutex<HashSet<ConnectionId>>>,
}

impl AdmittedDials {
    /// Announce that this connection id carries a valid admission.
    pub fn register(&self, id: ConnectionId) {
        self.lock().insert(id);
    }

    /// Consume the announcement, reporting whether there was one.
    pub fn take(&self, id: ConnectionId) -> bool {
        self.lock().remove(&id)
    }

    /// Drop an announcement that was never consumed.
    ///
    /// A dial libp2p refuses before reaching the hook leaves its id
    /// behind, and an id that stays is one a later dial could reuse
    /// without an admission. Cheap to call unconditionally, which is
    /// why the caller does.
    pub fn forget(&self, id: ConnectionId) {
        self.lock().remove(&id);
    }

    /// Ids currently announced. Zero everywhere except mid-dial.
    #[must_use]
    pub fn outstanding(&self) -> usize {
        self.lock().len()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashSet<ConnectionId>> {
        // Poisoning is recovered rather than propagated: the protected
        // value is a set of ids with no invariant spanning two
        // operations, so a panic elsewhere must not turn every future
        // dial into a denial.
        self.ids.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Denies any outbound dial the root admission did not issue.
#[derive(Debug, Default)]
pub struct OutboundAdmission {
    admitted: AdmittedDials,
}

impl OutboundAdmission {
    /// A handle to the set this behaviour consults.
    #[must_use]
    pub fn admitted(&self) -> AdmittedDials {
        self.admitted.clone()
    }
}

impl NetworkBehaviour for OutboundAdmission {
    type ConnectionHandler = dummy::ConnectionHandler;
    type ToSwarm = std::convert::Infallible;

    /// Every outbound dial, whoever asked for it.
    ///
    /// # Errors
    /// [`ConnectionDenied`] when this connection id carries no
    /// admission — which is every dial that did not come through
    /// `GatedSwarm::dial`.
    fn handle_pending_outbound_connection(
        &mut self,
        connection_id: ConnectionId,
        _peer: Option<PeerId>,
        _addresses: &[Multiaddr],
        _effective_role: Endpoint,
    ) -> Result<Vec<Multiaddr>, ConnectionDenied> {
        if self.admitted.take(connection_id) {
            // No extra addresses: the admitted ones are bound into the
            // DialOpts by `AdmittedDial`, and adding any here would be
            // this behaviour deciding where an admitted dial goes.
            Ok(Vec::new())
        } else {
            Err(ConnectionDenied::new(std::io::Error::other(REFUSAL)))
        }
    }

    fn handle_established_inbound_connection(
        &mut self,
        _connection_id: ConnectionId,
        _peer: PeerId,
        _local_addr: &Multiaddr,
        _remote_addr: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        Ok(dummy::ConnectionHandler)
    }

    fn handle_established_outbound_connection(
        &mut self,
        _connection_id: ConnectionId,
        _peer: PeerId,
        _addr: &Multiaddr,
        _role_override: Endpoint,
        _port_use: PortUse,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        Ok(dummy::ConnectionHandler)
    }

    fn on_swarm_event(&mut self, _event: FromSwarm<'_>) {}

    fn on_connection_handler_event(
        &mut self,
        _peer: PeerId,
        _connection: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        match event {}
    }

    fn poll(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use super::{AdmittedDials, OutboundAdmission};
    use libp2p::core::Endpoint;
    use libp2p::swarm::{ConnectionId, NetworkBehaviour};

    #[test]
    fn a_dial_nobody_admitted_is_denied() {
        // THE BEHAVIOUR PATH. Kademlia, AutoNAT, Relay and DCUtR all
        // dial from inside the Swarm, where the wrapper cannot see
        // them; this is the hook they go through instead. Written now,
        // while nothing dials, because CLAUDE.md forbids turning on an
        // autonomous behaviour and retrofitting the gate afterwards.
        let mut gate = OutboundAdmission::default();
        let denied = gate.handle_pending_outbound_connection(
            ConnectionId::new_unchecked(7),
            None,
            &[],
            Endpoint::Dialer,
        );
        assert!(denied.is_err(), "an unadmitted connection id must not dial");
    }

    #[test]
    fn an_admitted_dial_passes_exactly_once() {
        // ONCE, because the registration is consumed. An id that stayed
        // behind would let a later dial -- including a behaviour's --
        // reuse an admission that was already spent.
        let mut gate = OutboundAdmission::default();
        let admitted = gate.admitted();
        let id = ConnectionId::new_unchecked(11);
        admitted.register(id);
        assert_eq!(admitted.outstanding(), 1);

        assert!(
            gate.handle_pending_outbound_connection(id, None, &[], Endpoint::Dialer)
                .is_ok(),
            "the admitted dial proceeds"
        );
        assert_eq!(admitted.outstanding(), 0, "and the registration is spent");
        assert!(
            gate.handle_pending_outbound_connection(id, None, &[], Endpoint::Dialer)
                .is_err(),
            "the same id must not pass a second time"
        );
    }

    #[test]
    fn an_abandoned_registration_can_be_forgotten() {
        // libp2p can refuse a dial before the hook runs. The id would
        // otherwise sit in the set until something reused it.
        let admitted = AdmittedDials::default();
        let id = ConnectionId::new_unchecked(3);
        admitted.register(id);
        admitted.forget(id);
        assert_eq!(admitted.outstanding(), 0);
        assert!(!admitted.take(id), "nothing left to consume");
    }
}
