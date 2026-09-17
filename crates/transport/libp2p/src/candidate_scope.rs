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
//! it is told about and never shrinks. Identify reports one observed
//! address per connection, so a remote party with many connections --
//! or one lying peer with many claimed observations -- chooses how
//! large that map gets. This wrapper forwards at most
//! [`MAX_TRACKED_CANDIDATES`] DISTINCT addresses over its life, the
//! same bound the manager holds for the set it counts, and counts what
//! it refused past that. A re-report of an address already forwarded
//! passes, because the crate uses it as a score and it costs no entry.
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

use std::collections::BTreeSet;
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
    /// Every distinct address forwarded so far. Bounded at
    /// [`MAX_TRACKED_CANDIDATES`]; see the module note.
    forwarded: BTreeSet<String>,
    rejected: usize,
    truncated: usize,
    suppressed_confirmations: usize,
}

impl<B> ScopedCandidates<B> {
    /// Wrap `inner`.
    pub fn new(inner: B) -> Self {
        Self {
            inner,
            forwarded: BTreeSet::new(),
            rejected: 0,
            truncated: 0,
            suppressed_confirmations: 0,
        }
    }

    /// The wrapped behaviour, for the composed behaviour's own use.
    pub fn inner_mut(&mut self) -> &mut B {
        &mut self.inner
    }

    /// The probeable candidates the inner behaviour has been told
    /// about, in canonical string form -- the set the manager counts.
    pub fn candidates(&self) -> impl Iterator<Item = &str> {
        self.forwarded.iter().map(String::as_str)
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

    /// Whether `addr` may reach the inner behaviour, recording it if so.
    fn admit_candidate(&mut self, addr: &Multiaddr) -> bool {
        let text = addr.to_string();
        if !is_probeable_address(&text) {
            self.rejected += 1;
            return false;
        }
        if self.forwarded.contains(&text) {
            return true;
        }
        if self.forwarded.len() >= MAX_TRACKED_CANDIDATES {
            self.truncated += 1;
            return false;
        }
        self.forwarded.insert(text);
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
