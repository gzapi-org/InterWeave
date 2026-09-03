// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! Which behaviour asked for a dial, told to the gate before it decides.
//!
//! # The problem
//!
//! `NetworkBehaviour::handle_pending_outbound_connection` is handed a
//! `ConnectionId`, an `Option<PeerId>` and an address slice whose
//! contents depend on the caller. None of them names the behaviour that
//! asked. Stage 10 could infer it, because Kademlia was the only
//! behaviour compiled that dials, so "no root admission ticket" and
//! "Kademlia" were the same set.
//!
//! Stage 11 adds three more, and the inference then fails in the
//! direction that breaks the stack: `KademliaQuery.is_data_plane()` is
//! true, and `ConnectionPolicy::admit` refuses a data-plane origin for a
//! `ConnectivityInfrastructureOnly` peer — so every relay reservation
//! and every AutoNAT probe would be refused against exactly the
//! infrastructure the reachability stack exists to use. SPIKE-004 ran
//! the shipped gate in front of a real relay client and measured that
//! refusal (`kademlia dial refused: NotAuthorizedForDataPlane`), and
//! measured the same dial admitted when the relay's trust class was the
//! only thing changed.
//!
//! # The mechanism
//!
//! A behaviour computes its own `ConnectionId` before the Swarm ever
//! sees the dial: `DialOpts::build` allocates it and
//! `opts.connection_id()` reads it back. So a wrapper that observes its
//! inner behaviour's `ToSwarm::Dial` on the way past can record
//! `ConnectionId -> DialOrigin` BEFORE the gate is asked, and the gate
//! reads the note rather than guessing.
//!
//! [`Attributing`] is that wrapper and [`DialAttribution`] is the map.
//! The gate CONSUMES a note when it reads one, because a `ConnectionId`
//! names one dial attempt and a note that outlived its dial would be
//! read by the next.
//!
//! # Why a classifier rather than one origin per behaviour
//!
//! Giving each wrapper a fixed origin looks sufficient and is not.
//! `relay::client::Behaviour` dials for two different reasons — a
//! reservation with the relay, and, in a future crate version, a
//! circuit toward a destination — and those are different origins with
//! different admission answers. The classifier is per dial, so one
//! behaviour can answer both.
//!
//! # An unattributed dial is REFUSED
//!
//! Fail-closed is the only safe direction: a dial nobody claims cannot
//! be classified, and guessing is what this module exists to stop.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use interweave_transport_runtime::DialOrigin;
use libp2p::Multiaddr;
use libp2p::PeerId;
use libp2p::core::Endpoint;
use libp2p::core::transport::PortUse;
use libp2p::swarm::{
    ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, THandler, THandlerInEvent,
    THandlerOutEvent, ToSwarm,
};

/// Decides the origin of one dial from the peer it names.
///
/// Shared rather than owned because a wrapper lives inside the
/// composed behaviour while the configuration it consults — which
/// peers are this node's relays, say — is owned elsewhere and changes.
pub type Classifier = Arc<dyn Fn(Option<PeerId>) -> DialOrigin + Send + Sync>;

/// A classifier that answers the same origin for every dial.
#[must_use]
pub fn always(origin: DialOrigin) -> Classifier {
    Arc::new(move |_| origin)
}

/// What each behaviour-originated dial was for, keyed by the
/// `ConnectionId` the originating behaviour minted.
///
/// Bounded by the Swarm's own pending-dial accounting: an entry is
/// written when a behaviour emits `ToSwarm::Dial` and removed when the
/// gate reads it or the dial fails, both of which the Swarm guarantees
/// for every dial it accepts. The one path that writes without either
/// is a dial the Swarm refuses synchronously — before the pending hook
/// — which is why [`Attributing::on_swarm_event`] forgets on
/// `FromSwarm::DialFailure`.
#[derive(Debug, Clone, Default)]
pub struct DialAttribution {
    notes: Arc<Mutex<BTreeMap<ConnectionId, DialOrigin>>>,
}

impl DialAttribution {
    /// Record that `origin` is about to dial under `id`.
    pub fn announce(&self, id: ConnectionId, origin: DialOrigin) {
        self.lock().insert(id, origin);
    }

    /// Read and consume the note for `id`.
    ///
    /// Consuming, because a `ConnectionId` names one dial attempt: a
    /// note left behind would attribute the next dial to whatever made
    /// the last one.
    #[must_use]
    pub fn resolve(&self, id: ConnectionId) -> Option<DialOrigin> {
        self.lock().remove(&id)
    }

    /// Drop a note whose dial never reached the gate.
    ///
    /// Cheap to call unconditionally, which is why the wrapper does.
    pub fn forget(&self, id: ConnectionId) {
        self.lock().remove(&id);
    }

    /// Notes outstanding. Zero everywhere except mid-dial.
    #[must_use]
    pub fn outstanding(&self) -> usize {
        self.lock().len()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<ConnectionId, DialOrigin>> {
        // Poisoning is recovered rather than propagated, as in
        // `AdmittedDials`: the protected value is a map of ids with no
        // invariant spanning two operations, and a panic elsewhere must
        // not turn every future dial into a refusal.
        self.notes.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// A behaviour that announces its own dials.
///
/// Transparent in every other respect: every method forwards, and the
/// inner behaviour decides everything it decided before.
pub struct Attributing<B> {
    inner: B,
    classify: Classifier,
    attribution: DialAttribution,
}

impl<B> Attributing<B> {
    /// Wrap `inner`, announcing into `attribution`.
    pub fn new(inner: B, classify: Classifier, attribution: DialAttribution) -> Self {
        Self {
            inner,
            classify,
            attribution,
        }
    }

    /// The wrapped behaviour, for the composed behaviour's own use.
    pub fn inner_mut(&mut self) -> &mut B {
        &mut self.inner
    }
}

impl<B: NetworkBehaviour> NetworkBehaviour for Attributing<B> {
    type ConnectionHandler = B::ConnectionHandler;
    type ToSwarm = B::ToSwarm;

    fn handle_established_inbound_connection(
        &mut self,
        id: ConnectionId,
        peer: PeerId,
        local: &Multiaddr,
        remote: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        self.inner
            .handle_established_inbound_connection(id, peer, local, remote)
    }

    fn handle_established_outbound_connection(
        &mut self,
        id: ConnectionId,
        peer: PeerId,
        addr: &Multiaddr,
        role: Endpoint,
        port: PortUse,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        self.inner
            .handle_established_outbound_connection(id, peer, addr, role, port)
    }

    fn handle_pending_inbound_connection(
        &mut self,
        id: ConnectionId,
        local: &Multiaddr,
        remote: &Multiaddr,
    ) -> Result<(), ConnectionDenied> {
        self.inner
            .handle_pending_inbound_connection(id, local, remote)
    }

    fn handle_pending_outbound_connection(
        &mut self,
        id: ConnectionId,
        peer: Option<PeerId>,
        addresses: &[Multiaddr],
        role: Endpoint,
    ) -> Result<Vec<Multiaddr>, ConnectionDenied> {
        self.inner
            .handle_pending_outbound_connection(id, peer, addresses, role)
    }

    /// A dial that failed leaves a note the gate will never read.
    ///
    /// The Swarm refuses some dials before the pending hook — an unmet
    /// `PeerCondition` is the reachable one — and reports them as
    /// `FromSwarm::DialFailure` without ever asking the gate. Without
    /// this the note would stay, and the next dial to reuse that
    /// `ConnectionId` would inherit it.
    fn on_swarm_event(&mut self, event: FromSwarm<'_>) {
        if let FromSwarm::DialFailure(failure) = &event {
            self.attribution.forget(failure.connection_id);
        }
        self.inner.on_swarm_event(event);
    }

    fn on_connection_handler_event(
        &mut self,
        peer: PeerId,
        id: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        self.inner.on_connection_handler_event(peer, id, event);
    }

    /// The note is written HERE, before the Swarm acts on the dial.
    ///
    /// `DialOpts` already carries the `ConnectionId` the Swarm will use
    /// — `opts.connection_id()` is the same value the pending hook is
    /// later handed — so this is a correlation rather than a guess.
    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        let polled = self.inner.poll(cx);
        if let Poll::Ready(ToSwarm::Dial { opts }) = &polled {
            let origin = (self.classify)(opts.get_peer_id());
            self.attribution.announce(opts.connection_id(), origin);
        }
        polled
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use super::*;
    use libp2p::swarm::DialError;
    use libp2p::swarm::dummy;

    const PEER: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";

    fn id(n: usize) -> ConnectionId {
        ConnectionId::new_unchecked(n)
    }

    #[test]
    fn a_note_records_the_classifier_s_answer_and_resolve_consumes_it() {
        let a = DialAttribution::default();
        a.announce(id(1), DialOrigin::RelayReservation);
        assert_eq!(a.outstanding(), 1);
        assert_eq!(a.resolve(id(1)), Some(DialOrigin::RelayReservation));
        assert_eq!(a.outstanding(), 0);
        assert_eq!(a.resolve(id(1)), None, "a note is read once");
    }

    #[test]
    fn the_classifier_answers_per_dial_not_per_behaviour() {
        // The reason this is a closure and not a field: one behaviour
        // dials for two reasons. `relay::client` reserves WITH a relay
        // and, in a future crate version, opens a circuit TOWARD a
        // destination — different origins, different admission answers,
        // one behaviour.
        let relay: PeerId = PEER.parse().expect("valid PeerId");
        let classify: Classifier = Arc::new(move |peer| match peer {
            Some(p) if p == relay => DialOrigin::RelayReservation,
            _ => DialOrigin::RelayCircuit,
        });
        assert_eq!(classify(Some(relay)), DialOrigin::RelayReservation);
        assert_eq!(classify(None), DialOrigin::RelayCircuit);
        assert_eq!(
            always(DialOrigin::AutonatProbe)(None),
            DialOrigin::AutonatProbe
        );
    }

    /// THE CLEANUP THE WRAPPER'S OWN COMMENT CLAIMS.
    ///
    /// A dial the Swarm refuses before the pending hook — an unmet
    /// `PeerCondition` is the reachable case — is reported as
    /// `FromSwarm::DialFailure` and never reaches the gate. Its note
    /// would otherwise stay, and the next dial to reuse that
    /// `ConnectionId` would be classified as whatever made this one.
    #[test]
    fn a_dial_that_fails_before_the_gate_leaves_no_note_behind() {
        let attribution = DialAttribution::default();
        let mut wrapper = Attributing::new(
            dummy::Behaviour,
            always(DialOrigin::KademliaQuery),
            attribution.clone(),
        );
        attribution.announce(id(3), DialOrigin::KademliaQuery);
        assert_eq!(attribution.outstanding(), 1);

        let error = DialError::Aborted;
        wrapper.on_swarm_event(FromSwarm::DialFailure(libp2p::swarm::DialFailure {
            peer_id: None,
            error: &error,
            connection_id: id(3),
        }));
        assert_eq!(
            attribution.outstanding(),
            0,
            "the note for a dial that never reached the gate is dropped"
        );

        // AND ONLY THAT ONE. A sweep would be just as green here and
        // would drop notes for dials still in flight.
        attribution.announce(id(4), DialOrigin::KademliaQuery);
        attribution.announce(id(5), DialOrigin::AutonatProbe);
        let error = DialError::Aborted;
        wrapper.on_swarm_event(FromSwarm::DialFailure(libp2p::swarm::DialFailure {
            peer_id: None,
            error: &error,
            connection_id: id(4),
        }));
        assert_eq!(attribution.resolve(id(5)), Some(DialOrigin::AutonatProbe));
        assert_eq!(attribution.resolve(id(4)), None);
    }
}
