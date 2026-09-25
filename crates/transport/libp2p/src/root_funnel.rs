// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! ADR-0052 A 2026-09-25 D1: every address a BEHAVIOUR contributes to a
//! dial passes one place before a socket opens.
//!
//! # Why the root, and why a wrapper rather than another field
//!
//! A dial built with `extend_addresses_through_behaviour` -- Kademlia's
//! and the relay client's are -- gets its addresses from the behaviours,
//! not from whoever asked for it. `libp2p-swarm 0.48.0` `Swarm::dial`
//! calls the ROOT behaviour's `handle_pending_outbound_connection` exactly
//! once and extends the dial only from what that single call returns; on
//! `Err` the dial is `DialError::Denied`. A derived composite answers that
//! call by concatenating what each of its fields returns.
//!
//! So a hook that is one FIELD of the composite -- which is where
//! `OutboundAdmission` sits, and why its pending hook sees an empty list
//! for these dials (F9) -- cannot see what its siblings contribute. A
//! wrapper AROUND the composite sees the whole union, and is the one
//! place every behaviour-extended address passes. That is this type.
//!
//! # What it decides, and what it does not
//!
//! It prunes the EXTENSION -- the list the inner behaviour returns -- by
//! ADR-0052's class boundary, and nothing else:
//!
//! - The EXPLICIT addresses a dial was built with are the caller's, and
//!   rule 5 keeps them the caller's: a hook adds and never removes, and
//!   a refused explicit address is denied and reissued at the site that
//!   owns the dial. The Swarm passes them in as `addresses` and does not
//!   expect them back, so this wrapper never touches them.
//! - It is not admission. Whether a dial may happen at all -- to whom,
//!   under what origin, within which ceiling -- is `OutboundAdmission`'s,
//!   and still is.
//!
//! A pruned address is counted by class and never logged (rule 5). The
//! counts are read through [`RootFunnelCounterHandle`], which is held
//! outside the Swarm task, because a refusal nobody can read is not a
//! record of anything.
//!
//! # The rule it applies
//!
//! `is_advertised_address`, the same predicate the address book's
//! learn-site hook uses: a behaviour-contributed address is one a peer
//! supplied, and D3 requires the STORE and the DIAL to enforce the same
//! rule, or an address refused on its way into the book walks back in
//! through the routing table. Applied through [`OperatorSet::admits`],
//! so an address that came in by the operator's door passes whatever
//! its class (rule 9) -- the operator's `/dns4` seed, re-offered by
//! Kademlia, still resolves. Rule 3 needs this node's own listeners,
//! which this wrapper tracks from the Swarm's own listener events rather
//! than being told them, so it cannot be told stale ones.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll};

use libp2p::core::Endpoint;
use libp2p::core::transport::PortUse;
use libp2p::swarm::{
    ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, THandler, THandlerInEvent,
    THandlerOutEvent, ToSwarm,
};
use libp2p::{Multiaddr, PeerId};

use interweave_transport_runtime::reachability::literal_host;

use crate::operator_set::OperatorSet;

/// What the funnel has done, by class.
///
/// `candidates_removed` is ADR-0052 rule 5's own name, shared with the
/// explicit-list sites (the punch's), so one vocabulary covers every
/// place a refused address is taken out of a dial.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RootFunnelCounters {
    /// Behaviour-contributed addresses that passed and went to the dial.
    ///
    /// Counted beside the removals because without it a funnel that
    /// removes everything and a dial nobody extended read the same.
    pub passed: usize,
    /// Those removed, by `CandidateRefusal::label`.
    pub candidates_removed: BTreeMap<&'static str, usize>,
    /// Dials denied because the funnel removed every address a
    /// behaviour contributed and the dial carried none of its own.
    ///
    /// SPIKE-004 measured that the Swarm discards the denial of a
    /// behaviour-originated dial -- no `Dialing`, no
    /// `OutgoingConnectionError`, only the behaviour is told -- so this
    /// count is the only place such a denial is visible at all.
    pub dials_denied: usize,
}

impl RootFunnelCounters {
    /// Every removed address, whatever its class.
    #[must_use]
    pub fn candidates_removed_total(&self) -> usize {
        self.candidates_removed.values().sum()
    }
}

/// Why the funnel denied a dial: every address a behaviour offered was
/// outside ADR-0052's boundary, and the dial named none of its own.
///
/// The class of each removed address is in the counters; the addresses
/// themselves are in neither this error nor any log (rule 5).
#[derive(Debug)]
pub struct NothingDialable;

impl std::fmt::Display for NothingDialable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "every address a behaviour contributed is outside the peer-supplied address \
             boundary (ADR-0052), and the dial carried no explicit address",
        )
    }
}

impl std::error::Error for NothingDialable {}

/// A shared, readable view of [`RootFunnelCounters`].
///
/// Held by whoever built the Swarm, so the counts are readable while the
/// Swarm task owns the behaviour. The same shape as the hole-punch and
/// probe-server counter handles.
#[derive(Debug, Clone, Default)]
pub struct RootFunnelCounterHandle {
    inner: Arc<Mutex<RootFunnelCounters>>,
}

impl RootFunnelCounterHandle {
    /// The counters as they stand.
    #[must_use]
    pub fn snapshot(&self) -> RootFunnelCounters {
        self.lock().clone()
    }

    fn lock(&self) -> MutexGuard<'_, RootFunnelCounters> {
        // A panic while counting leaves a count, not a hazard: recover
        // the guard rather than turn a poisoned counter into a second
        // panic in the Swarm task.
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// The root of the composed behaviour, pruning what it extends a dial
/// with (ADR-0052 A 2026-09-25 D1).
pub struct RootFunnel<B> {
    inner: B,
    /// Every single-host literal address this node currently listens
    /// on, for rule 3. Bounded by the listeners this PROCESS binds --
    /// `max_active_listeners`, times its interfaces -- never by anything
    /// a remote party chooses.
    ///
    /// THAT BOUND IS WHY A CIRCUIT LISTENER IS NOT TAKEN. A relay
    /// reservation surfaces as a `NewListenAddr` whose address the RELAY
    /// reported, re-emitted on every renewal at an interval the relay
    /// sets; taking it let a relay grow this set at its chosen rate, and
    /// a relay reporting a 10/8 address made a host with only global
    /// listeners admit private candidates (#111 review P2-1).
    /// `a_relay_reservation_listener_is_not_this_nodes_listener` pins it.
    own_listeners: BTreeSet<Multiaddr>,
    counters: RootFunnelCounterHandle,
    /// What came in by the operator's door, admitted whatever its class
    /// (rule 9): the runtime's one set, shared.
    operator: OperatorSet,
}

impl<B> RootFunnel<B> {
    /// Wrap `inner`, consulting `operator` -- the runtime's one record of
    /// the operator's door -- before the class boundary (ADR-0052 rule 9).
    ///
    /// THE SET IS A CONSTRUCTOR ARGUMENT, not a setter with an empty
    /// default. It used to be `new(inner).with_operator_set(set)`, and
    /// dropping the second call left a funnel that refused the operator's
    /// own seeds with every test green -- the wiring had no test (#111
    /// review P2-4). A clone of the runtime's set, not a copy of its
    /// contents: an address the operator adds after the Swarm is built
    /// is admitted here from that moment.
    pub fn new(inner: B, operator: OperatorSet) -> Self {
        Self {
            inner,
            own_listeners: BTreeSet::new(),
            counters: RootFunnelCounterHandle::default(),
            operator,
        }
    }

    /// A readable handle on the counts, for whoever holds the Swarm.
    #[must_use]
    pub fn counters(&self) -> RootFunnelCounterHandle {
        self.counters.clone()
    }

    /// The composed behaviour this funnel wraps.
    pub const fn inner(&self) -> &B {
        &self.inner
    }

    /// The composed behaviour this funnel wraps, mutably.
    pub fn inner_mut(&mut self) -> &mut B {
        &mut self.inner
    }

    /// The extension with every refused address removed, and whether
    /// anything was.
    fn prune(&self, extended: Vec<Multiaddr>) -> (Vec<Multiaddr>, bool) {
        let listeners: Vec<String> = self.own_listeners.iter().map(ToString::to_string).collect();
        let mut counts = self.counters.lock();
        let mut removed_any = false;
        let kept = extended
            .into_iter()
            .filter(|address| {
                match self
                    .operator
                    .admits(address, listeners.iter().map(String::as_str))
                {
                    Ok(()) => {
                        counts.passed += 1;
                        true
                    }
                    Err(class) => {
                        *counts.candidates_removed.entry(class.label()).or_default() += 1;
                        removed_any = true;
                        false
                    }
                }
            })
            .collect();
        (kept, removed_any)
    }
}

impl<B: NetworkBehaviour> NetworkBehaviour for RootFunnel<B> {
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

    /// THE FUNNEL. The inner composite's answer is the whole extension;
    /// what survives the class boundary is what the Swarm dials.
    ///
    /// `addresses` -- the dial's explicit list -- is handed through
    /// untouched and is not part of the return: the Swarm keeps it and
    /// appends what this returns.
    ///
    /// DENIED ONLY WHEN THIS FUNNEL EMPTIED THE DIAL: something was
    /// removed, nothing survived, and the dial carried no address of its
    /// own (ADR-0052 rule 5, A 2026-09-25). A denial is told to the
    /// behaviour that asked, where an empty return would surface as a
    /// bare `NoAddresses` that says nothing about why. A dial no
    /// behaviour contributed anything to is passed through unchanged:
    /// this funnel did not cause its emptiness, and denying it would
    /// turn every such dial into a refusal it did not make.
    fn handle_pending_outbound_connection(
        &mut self,
        id: ConnectionId,
        peer: Option<PeerId>,
        addresses: &[Multiaddr],
        role: Endpoint,
    ) -> Result<Vec<Multiaddr>, ConnectionDenied> {
        let extended = self
            .inner
            .handle_pending_outbound_connection(id, peer, addresses, role)?;
        let (kept, removed_any) = self.prune(extended);
        if removed_any && kept.is_empty() && addresses.is_empty() {
            self.counters.lock().dials_denied += 1;
            return Err(ConnectionDenied::new(NothingDialable));
        }
        Ok(kept)
    }

    fn on_swarm_event(&mut self, event: FromSwarm<'_>) {
        match &event {
            FromSwarm::NewListenAddr(e) if literal_host(&e.addr.to_string()).is_some() => {
                self.own_listeners.insert(e.addr.clone());
            }
            FromSwarm::ExpiredListenAddr(e) => {
                self.own_listeners.remove(e.addr);
            }
            _ => {}
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

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        self.inner.poll(cx)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use super::*;
    use libp2p::core::transport::ListenerId;
    use libp2p::swarm::{NewListenAddr, dummy};

    /// A composite that extends every dial with `offered`.
    struct Offering {
        offered: Vec<Multiaddr>,
    }

    impl NetworkBehaviour for Offering {
        type ConnectionHandler = dummy::ConnectionHandler;
        type ToSwarm = ();

        fn handle_established_inbound_connection(
            &mut self,
            _: ConnectionId,
            _: PeerId,
            _: &Multiaddr,
            _: &Multiaddr,
        ) -> Result<THandler<Self>, ConnectionDenied> {
            Ok(dummy::ConnectionHandler)
        }

        fn handle_established_outbound_connection(
            &mut self,
            _: ConnectionId,
            _: PeerId,
            _: &Multiaddr,
            _: Endpoint,
            _: PortUse,
        ) -> Result<THandler<Self>, ConnectionDenied> {
            Ok(dummy::ConnectionHandler)
        }

        fn handle_pending_outbound_connection(
            &mut self,
            _: ConnectionId,
            _: Option<PeerId>,
            _: &[Multiaddr],
            _: Endpoint,
        ) -> Result<Vec<Multiaddr>, ConnectionDenied> {
            Ok(self.offered.clone())
        }

        fn on_swarm_event(&mut self, _: FromSwarm<'_>) {}

        fn on_connection_handler_event(
            &mut self,
            _: PeerId,
            _: ConnectionId,
            _: THandlerOutEvent<Self>,
        ) {
        }

        fn poll(&mut self, _: &mut Context<'_>) -> Poll<ToSwarm<(), THandlerInEvent<Self>>> {
            Poll::Pending
        }
    }

    fn addrs(list: &[&str]) -> Vec<Multiaddr> {
        list.iter().map(|a| a.parse().expect("valid")).collect()
    }

    fn funnel(offered: &[&str]) -> RootFunnel<Offering> {
        RootFunnel::new(
            Offering {
                offered: addrs(offered),
            },
            OperatorSet::new(),
        )
    }

    fn listen(f: &mut RootFunnel<Offering>, address: &str) {
        let addr: Multiaddr = address.parse().expect("valid");
        f.on_swarm_event(FromSwarm::NewListenAddr(NewListenAddr {
            listener_id: ListenerId::next(),
            addr: &addr,
        }));
    }

    fn dial(
        f: &mut RootFunnel<Offering>,
        explicit: &[&str],
    ) -> Result<Vec<Multiaddr>, ConnectionDenied> {
        f.handle_pending_outbound_connection(
            ConnectionId::new_unchecked(1),
            Some(PeerId::random()),
            &addrs(explicit),
            Endpoint::Dialer,
        )
    }

    /// #111 review P2-1. A relay-reported private reservation address is
    /// not this node's listener: a private candidate stays refused beside
    /// it, and the set does not grow with it. THE CONTROL is a real
    /// private listener, beside which the same candidate passes.
    #[test]
    fn a_relay_reservation_listener_is_not_this_nodes_listener() {
        const R: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
        let mut f = funnel(&["/ip4/10.0.0.9/tcp/4001"]);
        listen(&mut f, "/ip4/203.0.113.9/tcp/4001");
        for renewal in 1..=3 {
            listen(
                &mut f,
                &format!("/ip4/10.0.0.{renewal}/tcp/4001/p2p/{R}/p2p-circuit/p2p/{R}"),
            );
        }
        assert_eq!(f.own_listeners.len(), 1, "only the process's own listener");
        assert!(
            dial(&mut f, &[]).is_err(),
            "the private candidate is refused"
        );

        listen(&mut f, "/ip4/10.0.0.2/tcp/4001");
        assert_eq!(
            dial(&mut f, &[]).expect("admitted"),
            addrs(&["/ip4/10.0.0.9/tcp/4001"]),
            "the control: beside a real private listener it passes"
        );
    }

    /// #111 review P2-5: each narrowing condition of the denial, alone.
    /// Denied only when the funnel removed something, nothing survived
    /// AND the dial named no address of its own.
    #[test]
    fn the_funnel_denies_only_a_dial_it_emptied_itself() {
        // Nothing contributed: not the funnel's emptiness.
        let mut f = funnel(&[]);
        assert_eq!(
            dial(&mut f, &[]).expect("passed through"),
            Vec::<Multiaddr>::new()
        );
        assert_eq!(f.counters().snapshot().dials_denied, 0);

        // Everything contributed refused, but the dial has its own.
        let mut f = funnel(&["/ip4/127.0.0.1/tcp/1"]);
        assert_eq!(
            dial(&mut f, &["/ip4/8.8.8.8/tcp/1"]).expect("the explicit address stands"),
            Vec::<Multiaddr>::new()
        );
        assert_eq!(f.counters().snapshot().dials_denied, 0);

        // Something survived.
        let mut f = funnel(&["/ip4/127.0.0.1/tcp/1", "/ip4/8.8.8.8/tcp/1"]);
        assert_eq!(
            dial(&mut f, &[]).expect("the survivor is dialled"),
            addrs(&["/ip4/8.8.8.8/tcp/1"])
        );
        assert_eq!(f.counters().snapshot().dials_denied, 0);

        // All three hold: denied, counted, and told why.
        let mut f = funnel(&["/ip4/127.0.0.1/tcp/1"]);
        let denied = dial(&mut f, &[]).expect_err("the funnel emptied it");
        assert!(denied.downcast::<NothingDialable>().is_ok());
        assert_eq!(f.counters().snapshot().dials_denied, 1);
    }
}
