// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `AUTONAT.md` §6 and §5 at the behaviour boundary: what the AutoNAT
//! client is allowed to probe, and who gets to say an address is
//! confirmed.
//!
//! # Why a wrapper, and not the manager
//!
//! The pinned client keeps its own candidate set, and it is fed by the
//! Swarm: every `FromSwarm::NewExternalAddrCandidate` reaches every
//! behaviour, and `libp2p-identify` emits one for every address a peer
//! CLAIMS to have observed us on. The client probes from exactly that
//! set -- the "arbitrary remote-supplied addresses" §6 forbids sending
//! to a server -- and `ReachabilityManager` never sees the set at all:
//! it refuses to COUNT a private address after the probe was sent
//! (its module note says "one step later"). Stopping the SEND has to
//! happen where the candidate enters, and that is here: a wrapper that
//! forwards every other event untouched and drops a candidate
//! [`is_probeable_address`] refuses.
//!
//! # Bounded, because the crate's map is not
//!
//! The client's `address_candidates` grows with every distinct address
//! it is told about and never shrinks. Identify reports every changed
//! observed address on a connection, so a remote party -- one lying
//! peer with many claimed observations is enough -- chooses how large
//! that map gets. This wrapper holds two sets, each bounded at
//! [`MAX_TRACKED_CANDIDATES`]: the addresses THIS PROFILE BOUND, which
//! the driver offers through [`ScopedCandidates::offer_listener`], and
//! the addresses PEERS CLAIMED TO OBSERVE, which arrive from the Swarm.
//! Two sets rather than one, because one arrival-ordered set is a
//! quota an authorized peer can spend on this profile's behalf: sixty-
//! four public-looking claims and the listener bound afterwards would
//! have been refused for the process's life. [`candidates`] yields the
//! bound set first, which is the order `ReachabilityManager::
//! set_candidates` was written to receive. A re-report of an address
//! already forwarded passes, because the crate uses it as a score and
//! it costs no entry; a distinct address past a set's bound is refused
//! and counted. The bound set starts over when the driver says the
//! listener set changed; the observed set is pruned of claims older
//! than the evidence TTL, so a quota one peer spent is free again once
//! its claims have aged out. What that bounds, stated rather than
//! implied: the crate's own map cannot shrink, so an authorized peer
//! that keeps claiming fresh addresses grows it by at most
//! [`MAX_TRACKED_CANDIDATES`] entries per TTL -- a slow, trust-gated
//! growth, chosen over a lifetime quota that the same peer could spend
//! once to keep this profile's real public address out for good.
//!
//! [`candidates`]: ScopedCandidates::candidates
//!
//! # The crate's own confirmation is not §5's
//!
//! On one server's success the client pushes
//! `ToSwarm::ExternalAddrConfirmed`, and the Swarm then advertises the
//! address through Identify. `AUTONAT.md` §5 confirms nothing on one
//! observer: `verified_public` needs the configured number of DISTINCT
//! servers, and it is the manager's verdict that adds the external
//! address and withdraws it when the evidence lapses (ADR-0051 names
//! the withdrawal as the adapter's, since the crate never emits
//! `ExternalAddrExpired`). So the confirmation is swallowed here and
//! counted; the runtime adds and removes external addresses from the
//! verdict alone. Pinned by `the_crates_own_confirmation_never_reaches_the_swarm`.

use std::collections::{BTreeMap, BTreeSet};
use std::task::{Context, Poll};

use interweave_transport_runtime::reachability::{MAX_TRACKED_CANDIDATES, is_probeable_address};
use libp2p::Multiaddr;
use libp2p::PeerId;
use libp2p::core::Endpoint;
use libp2p::core::transport::PortUse;
use libp2p::swarm::{
    ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, NewExternalAddrCandidate,
    THandler, THandlerInEvent, THandlerOutEvent, ToSwarm,
};

/// A behaviour whose external-address candidates are scoped by
/// `AUTONAT.md` §6 and whose confirmations are the manager's to make.
///
/// Transparent in every other respect: every method forwards, and the
/// inner behaviour decides everything else it decided before.
pub struct ScopedCandidates<B> {
    inner: B,
    /// Addresses this profile bound, offered by the driver. Bounded at
    /// [`MAX_TRACKED_CANDIDATES`]; see the module note.
    listeners: BTreeSet<String>,
    /// Addresses peers claimed to observe, from the Swarm, with the
    /// clock reading at their last claim. Bounded separately at
    /// [`MAX_TRACKED_CANDIDATES`] and pruned by age.
    observed: BTreeMap<String, u64>,
    /// The runtime's clock as of the last tick; a claim arriving between
    /// ticks is stamped with it, at most a tick stale.
    clock_ms: u64,
    rejected: usize,
    truncated: usize,
    suppressed_confirmations: usize,
}

impl<B> ScopedCandidates<B> {
    /// Wrap `inner`.
    pub fn new(inner: B) -> Self {
        Self {
            inner,
            listeners: BTreeSet::new(),
            observed: BTreeMap::new(),
            clock_ms: 0,
            rejected: 0,
            truncated: 0,
            suppressed_confirmations: 0,
        }
    }

    /// The listener set changed: forget the bound addresses, so what
    /// this profile now binds is offered afresh. The counters survive;
    /// they are the process's.
    pub fn reset_listeners(&mut self) {
        self.listeners.clear();
    }

    /// Advance the clock and forget every observed claim older than
    /// `ttl_ms`; the driver calls this on its tick.
    pub fn prune_observed(&mut self, now_ms: u64, ttl_ms: u64) {
        self.clock_ms = now_ms;
        self.observed
            .retain(|_, seen| seen.saturating_add(ttl_ms) > now_ms);
    }

    /// The wrapped behaviour, for the composed behaviour's own use.
    pub fn inner_mut(&mut self) -> &mut B {
        &mut self.inner
    }

    /// The probeable candidates the inner behaviour has been told
    /// about this epoch, bound addresses first, then observed ones --
    /// the set and the order the manager counts.
    pub fn candidates(&self) -> impl Iterator<Item = &str> {
        self.listeners
            .iter()
            .chain(
                self.observed
                    .keys()
                    .filter(|a| !self.listeners.contains(*a)),
            )
            .map(String::as_str)
    }

    /// Candidates dropped because [`is_probeable_address`] said no.
    #[must_use]
    pub const fn rejected(&self) -> usize {
        self.rejected
    }

    /// Probeable, distinct candidates dropped because the bound was
    /// already full.
    #[must_use]
    pub const fn truncated(&self) -> usize {
        self.truncated
    }

    /// Confirmations the crate emitted and this wrapper swallowed.
    #[must_use]
    pub const fn suppressed_confirmations(&self) -> usize {
        self.suppressed_confirmations
    }

    /// Whether `addr` may reach the inner behaviour as an observed
    /// candidate, recording it if so.
    fn admit_candidate(&mut self, addr: &Multiaddr) -> bool {
        let text = addr.to_string();
        if !is_probeable_address(&text) {
            self.rejected += 1;
            return false;
        }
        if self.listeners.contains(&text) {
            return true;
        }
        if let Some(seen) = self.observed.get_mut(&text) {
            *seen = self.clock_ms;
            return true;
        }
        if self.observed.len() >= MAX_TRACKED_CANDIDATES {
            self.truncated += 1;
            return false;
        }
        self.observed.insert(text, self.clock_ms);
        true
    }
}

impl<B: NetworkBehaviour> ScopedCandidates<B> {
    /// Offer an address THIS PROFILE BOUND as a candidate, through the
    /// same door the Swarm's observed candidates use and under the
    /// same rule, but against the listener set's own bound -- so a
    /// peer's claims cannot crowd out what this profile knows it
    /// listens on. Returns whether the client was told.
    pub fn offer_listener(&mut self, addr: &Multiaddr) -> bool {
        let text = addr.to_string();
        if !is_probeable_address(&text) {
            self.rejected += 1;
            return false;
        }
        if !self.listeners.contains(&text) {
            if self.listeners.len() >= MAX_TRACKED_CANDIDATES {
                self.truncated += 1;
                return false;
            }
            self.listeners.insert(text);
        }
        self.inner
            .on_swarm_event(FromSwarm::NewExternalAddrCandidate(
                NewExternalAddrCandidate { addr },
            ));
        true
    }
}

impl<B: NetworkBehaviour> NetworkBehaviour for ScopedCandidates<B> {
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

    /// A candidate outside §6's scope, or past the bound, stops here.
    fn on_swarm_event(&mut self, event: FromSwarm<'_>) {
        if let FromSwarm::NewExternalAddrCandidate(NewExternalAddrCandidate { addr }) = &event
            && !self.admit_candidate(addr)
        {
            return;
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

    /// The crate's confirmation is swallowed; everything else passes.
    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        loop {
            match self.inner.poll(cx) {
                Poll::Ready(ToSwarm::ExternalAddrConfirmed(_)) => {
                    self.suppressed_confirmations += 1;
                }
                other => return other,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use super::*;
    use libp2p::autonat::v2::client::Behaviour as Client;
    use libp2p::swarm::dummy;
    use std::collections::VecDeque;

    fn candidate(addr: &Multiaddr) -> FromSwarm<'_> {
        FromSwarm::NewExternalAddrCandidate(NewExternalAddrCandidate { addr })
    }

    /// Whether the real client was told about `addr`: `validate_addr`
    /// moves a KNOWN candidate to `Received`, after which `retest` --
    /// the vendored patch -- answers `true`; an unknown address stays
    /// unknown and `retest` answers `false`.
    fn client_knows(client: &mut Client, addr: &Multiaddr) -> bool {
        client.validate_addr(addr);
        client.retest(addr)
    }

    #[test]
    fn a_private_relayed_or_non_literal_candidate_never_reaches_the_client() {
        let mut scoped = ScopedCandidates::new(Client::default());
        // A public literal -- not a documentation range, which the rule
        // refuses: an earlier draft used 203.0.113.9 as the control and
        // the wrapper correctly refused its own control.
        let public: Multiaddr = "/ip4/8.8.8.8/tcp/4001".parse().expect("a literal");
        let refused: [Multiaddr; 4] = [
            "/ip4/10.0.0.1/tcp/4001".parse().expect("a literal"),
            "/ip6/fe80::1/tcp/4001".parse().expect("a literal"),
            "/ip4/8.8.8.8/tcp/4001/p2p/12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN/p2p-circuit"
                .parse()
                .expect("a literal"),
            "/dns4/example.invalid/tcp/4001".parse().expect("a literal"),
        ];
        // The CONTROL, first: a public literal is forwarded and the
        // client knows it, so the refusals below are refusals and not a
        // wrapper that forwards nothing.
        scoped.on_swarm_event(candidate(&public));
        assert!(client_knows(scoped.inner_mut(), &public));
        for addr in &refused {
            scoped.on_swarm_event(candidate(addr));
            assert!(
                !client_knows(scoped.inner_mut(), addr),
                "{addr} must not reach the client"
            );
        }
        assert_eq!(scoped.rejected(), refused.len());
        assert_eq!(scoped.truncated(), 0);
        assert_eq!(
            scoped.candidates().collect::<Vec<_>>(),
            [public.to_string()]
        );
    }

    #[test]
    fn a_full_observed_set_cannot_crowd_out_a_bound_listener_and_claims_age_out() {
        let mut scoped = ScopedCandidates::new(Client::default());
        // A peer fills the observed quota with public-looking claims.
        let claimed: Vec<Multiaddr> = (1..=MAX_TRACKED_CANDIDATES)
            .map(|i| {
                format!("/ip4/1.0.{}.{}/tcp/4001", i / 250, 1 + i % 250)
                    .parse()
                    .expect("a literal")
            })
            .collect();
        for addr in &claimed {
            scoped.on_swarm_event(candidate(addr));
        }
        assert_eq!(scoped.truncated(), 0);
        // A listener bound AFTER the fill is still forwarded: its own
        // quota, not the peers'.
        let bound: Multiaddr = "/ip4/8.8.8.8/tcp/4001".parse().expect("a literal");
        assert!(scoped.offer_listener(&bound));
        assert!(client_knows(scoped.inner_mut(), &bound));
        // And it comes FIRST in the order the manager receives.
        assert_eq!(scoped.candidates().next(), Some(bound.to_string().as_str()));
        assert_eq!(scoped.candidates().count(), MAX_TRACKED_CANDIDATES + 1);
        // One more observed claim is refused and counted.
        let extra: Multiaddr = "/ip4/9.9.9.9/tcp/4001".parse().expect("a literal");
        scoped.on_swarm_event(candidate(&extra));
        assert!(!client_knows(scoped.inner_mut(), &extra));
        assert_eq!(scoped.truncated(), 1);
        // A listener reset forgets the bound set only.
        scoped.reset_listeners();
        assert_eq!(scoped.candidates().count(), MAX_TRACKED_CANDIDATES);
        // The claims age out: a prune past the TTL frees the quota,
        // and the refused claim now passes. A re-claim first refreshes
        // one entry, which survives the prune -- the control that the
        // prune is by age and not a reset.
        scoped.prune_observed(1_000, 10_000);
        scoped.on_swarm_event(candidate(&claimed[0]));
        // The others were stamped at clock 0 and expire at 10_000; the
        // refreshed one at 1_000 expires at 11_000, so at 10_999 it is
        // the only survivor -- and at 11_000 it is gone too.
        scoped.prune_observed(10_999, 10_000);
        assert_eq!(
            scoped.candidates().collect::<Vec<_>>(),
            [claimed[0].to_string()]
        );
        scoped.prune_observed(11_000, 10_000);
        assert_eq!(scoped.candidates().count(), 0);
        scoped.on_swarm_event(candidate(&extra));
        assert!(client_knows(scoped.inner_mut(), &extra));
        assert_eq!(scoped.truncated(), 1, "the counter is the process's");
        // A private listener is refused like any other candidate.
        let private: Multiaddr = "/ip4/127.0.0.1/tcp/4001".parse().expect("a literal");
        assert!(!scoped.offer_listener(&private));
        assert!(!client_knows(scoped.inner_mut(), &private));
    }

    #[test]
    fn the_forwarded_set_is_bounded_and_a_repeat_costs_no_entry() {
        let mut scoped = ScopedCandidates::new(Client::default());
        // 1.0.0.1 .. 1.0.0.N are public; one past the bound is refused.
        let addrs: Vec<Multiaddr> = (1..=MAX_TRACKED_CANDIDATES + 1)
            .map(|i| {
                format!("/ip4/1.0.{}.{}/tcp/4001", i / 250, 1 + i % 250)
                    .parse()
                    .expect("a literal")
            })
            .collect();
        for addr in &addrs[..MAX_TRACKED_CANDIDATES] {
            scoped.on_swarm_event(candidate(addr));
        }
        assert_eq!(scoped.candidates().count(), MAX_TRACKED_CANDIDATES);
        // A repeat of a forwarded address passes (the crate scores it)
        // and takes no entry.
        scoped.on_swarm_event(candidate(&addrs[0]));
        assert_eq!(scoped.candidates().count(), MAX_TRACKED_CANDIDATES);
        assert_eq!(scoped.truncated(), 0);
        // One more distinct address is refused and the client never
        // learns it.
        let extra = &addrs[MAX_TRACKED_CANDIDATES];
        scoped.on_swarm_event(candidate(extra));
        assert!(!client_knows(scoped.inner_mut(), extra));
        assert_eq!(scoped.truncated(), 1);
        assert_eq!(scoped.rejected(), 0);
        // And the control: the last one INSIDE the bound is known.
        assert!(client_knows(
            scoped.inner_mut(),
            &addrs[MAX_TRACKED_CANDIDATES - 1]
        ));
    }

    /// A behaviour that emits whatever it was queued, so the wrapper's
    /// poll can be watched without a probe.
    struct Emitter {
        queued: VecDeque<ToSwarm<(), std::convert::Infallible>>,
    }

    impl NetworkBehaviour for Emitter {
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

        fn on_swarm_event(&mut self, _: FromSwarm<'_>) {}

        fn on_connection_handler_event(
            &mut self,
            _: PeerId,
            _: ConnectionId,
            _: THandlerOutEvent<Self>,
        ) {
        }

        fn poll(
            &mut self,
            _: &mut Context<'_>,
        ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
            match self.queued.pop_front() {
                Some(ToSwarm::ExternalAddrConfirmed(a)) => {
                    Poll::Ready(ToSwarm::ExternalAddrConfirmed(a))
                }
                Some(ToSwarm::GenerateEvent(())) => Poll::Ready(ToSwarm::GenerateEvent(())),
                Some(ToSwarm::NewExternalAddrCandidate(a)) => {
                    Poll::Ready(ToSwarm::NewExternalAddrCandidate(a))
                }
                Some(_) | None => Poll::Pending,
            }
        }
    }

    #[test]
    fn the_crates_own_confirmation_never_reaches_the_swarm() {
        let addr: Multiaddr = "/ip4/203.0.113.9/tcp/4001".parse().expect("a literal");
        let mut scoped = ScopedCandidates::new(Emitter {
            queued: VecDeque::from([
                ToSwarm::ExternalAddrConfirmed(addr.clone()),
                ToSwarm::ExternalAddrConfirmed(addr.clone()),
                // THE CONTROL: an event of another kind passes, so the
                // wrapper is filtering a variant rather than everything.
                ToSwarm::GenerateEvent(()),
                ToSwarm::NewExternalAddrCandidate(addr),
            ]),
        });
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        assert!(matches!(
            scoped.poll(&mut cx),
            Poll::Ready(ToSwarm::GenerateEvent(()))
        ));
        assert_eq!(scoped.suppressed_confirmations(), 2);
        assert!(matches!(
            scoped.poll(&mut cx),
            Poll::Ready(ToSwarm::NewExternalAddrCandidate(_))
        ));
        assert!(matches!(scoped.poll(&mut cx), Poll::Pending));
        assert_eq!(scoped.suppressed_confirmations(), 2);
    }
}
