// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! ADR-0052 rule 9 (A 2026-09-25): provenance is a property of the
//! DOOR, never a tag on the address.
//!
//! Two doors admit an address into this runtime. The operator's --
//! profile configuration and the operator's own `AddAddress` command --
//! and the peer's: everything a behaviour learns or a peer asserts. An
//! address that came in by the operator's door is outside the
//! peer-supplied boundary, because no peer chose it: the operator's
//! `/dns4` bootstrap seed must resolve, the operator's LAN seed must
//! route. Everything else meets the floor.
//!
//! # Why a set, and not a field on the address
//!
//! No carrier gains a provenance field -- not `OfferRoutingPeer`, not
//! `AddAddress`, not `CandidatePeer`. A field a peer's path could set is
//! a field a peer's path will set; the door is what a peer cannot
//! choose. So the runtime keeps ONE record of what entered by the
//! operator's door, and every place that applies the boundary consults
//! it: each store's learn site and the root funnel, through
//! [`OperatorSet::admits`], so the exception is spelled once.
//!
//! # The key, and the pair it must not split
//!
//! Keyed by [`strip_peer_suffix`] at BOTH ends, the write and the read.
//! An operator writes a bootstrap address with its `/p2p/` suffix as
//! often as not; a behaviour hands the funnel the same route with or
//! without one. Two canonicalisations would make one route two keys,
//! and the operator's own seed would be refused at the door that
//! spelled it differently -- the defect this repository has already
//! shipped once on another key pair.
//!
//! Keyed by the ADDRESS, not by (peer, address): the rule's own words
//! are "an address in that set". A peer re-advertising the operator's
//! name for itself gains nothing an oracle needs -- the name is the
//! operator's, not the peer's choice -- and a dial that reaches the
//! wrong identity fails Noise and is quarantined as ADR-0011 already
//! says.

use std::collections::BTreeSet;
use std::sync::{Arc, RwLock};

use interweave_transport_runtime::reachability::{
    CandidateRefusal, is_advertised_address, is_discovered_address,
};
use libp2p::multiaddr::Protocol;
use libp2p::{Multiaddr, PeerId};

use crate::outbound_gate::strip_peer_suffix;

/// Addresses the operator's door can hold at once.
///
/// BOUNDED although the operator is trusted, because an unbounded
/// structure needs an architecture amendment here whoever fills it. The
/// operator's inputs are a profile's static peers, servers and relays
/// and whatever the operator adds by command; a profile's own limits
/// keep the first well under this, and an `AddAddress` past it is
/// refused and counted rather than silently dropping an earlier seed.
pub const MAX_OPERATOR_ADDRESSES: usize = 1024;

/// The shared record of what entered by the operator's door.
///
/// Cloned into every place that applies the boundary; every clone reads
/// the one set.
#[derive(Debug, Clone, Default)]
pub struct OperatorSet {
    inner: Arc<RwLock<Inner>>,
}

#[derive(Debug, Default)]
struct Inner {
    addresses: BTreeSet<String>,
    refused_full: usize,
}

impl OperatorSet {
    /// An empty set: nothing has come in by the operator's door yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that `address` came in by the operator's door.
    ///
    /// Returns `false` when the set is already at
    /// [`MAX_OPERATOR_ADDRESSES`] and `address` is not in it; the
    /// refusal is counted ([`OperatorSet::refused_full`]). Recording an
    /// address already held is a success and costs nothing.
    pub fn insert(&self, address: &Multiaddr) -> bool {
        let key = strip_peer_suffix(address);
        let mut inner = self.write();
        if inner.addresses.contains(&key) {
            return true;
        }
        if inner.addresses.len() >= MAX_OPERATOR_ADDRESSES {
            inner.refused_full += 1;
            return false;
        }
        inner.addresses.insert(key);
        true
    }

    /// Did `address` come in by the operator's door?
    #[must_use]
    pub fn contains(&self, address: &Multiaddr) -> bool {
        self.read().addresses.contains(&strip_peer_suffix(address))
    }

    /// The boundary every learn site and the root funnel apply: an
    /// operator's address is admitted whatever its class, and every
    /// other address meets ADR-0052's floor with rule 3.
    ///
    /// ONE FUNCTION so the operator exception cannot be applied at one
    /// door and forgotten at the next -- the half-closed shape the #111
    /// review found twice.
    ///
    /// # Errors
    /// The class a non-operator address was refused for.
    pub fn admits<'a>(
        &self,
        address: &Multiaddr,
        own_listeners: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), CandidateRefusal> {
        if self.contains(address) {
            return Ok(());
        }
        is_advertised_address(&address.to_string(), own_listeners)
    }

    /// [`OperatorSet::admits`] for a route a peer advertised about ITSELF
    /// into the address book -- the one store where a circuit is a route
    /// rather than a wrong answer (ADR-0052 A 2026-09-25, rule 8's
    /// Identify instance; ADR-0011 §Identify).
    ///
    /// A non-circuit address is judged exactly as [`OperatorSet::admits`]
    /// judges it. A circuit is the only route to a NATed peer, and
    /// refusing it deleted that route and protected nothing (#111 DNS
    /// review). It is admitted when:
    ///
    /// - its transport PREFIX -- everything before `/p2p-circuit` --
    ///   ends in the relay's `/p2p/<id>` (`libp2p-relay 0.22.0`'s client
    ///   refuses a circuit without one) and meets the floor or is the
    ///   operator's, so a peer-named relay's name is refused and an
    ///   operator-configured one resolves; and
    /// - what follows `/p2p-circuit` is nothing or `/p2p/<advertiser>`.
    ///   Anything else -- another peer, a second circuit -- is
    ///   [`CandidateRefusal::NotOwnCircuit`].
    ///
    /// Whether this node may dial THROUGH that relay is not asked here:
    /// that is the gate's question at dial time, under `RelayCircuit`.
    /// The Kademlia stash keeps [`OperatorSet::admits`], which refuses
    /// every circuit; this is the book's door alone.
    ///
    /// # Errors
    /// The class the route was refused for.
    pub fn admits_own_route<'a>(
        &self,
        address: &Multiaddr,
        advertiser: &PeerId,
        own_listeners: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), CandidateRefusal> {
        // THE OPERATOR'S DOOR FIRST, for the whole route: an address in
        // the set is admitted at every store door whatever its class
        // (rule 9). Judging only the relay prefix refused an operator's
        // own `AddAddress` circuit through a named relay as `not_literal`,
        // since the set holds the circuit and not the relay's route
        // (#111 post-merge re-review, finding 1).
        if self.contains(address) {
            return Ok(());
        }
        let parts: Vec<Protocol<'_>> = address.iter().collect();
        let Some(at) = parts.iter().position(|p| matches!(p, Protocol::P2pCircuit)) else {
            return self.admits(address, own_listeners);
        };
        let own_tail = match &parts[at + 1..] {
            [] => true,
            [Protocol::P2p(id)] => id == advertiser,
            _ => false,
        };
        if !own_tail {
            return Err(CandidateRefusal::NotOwnCircuit);
        }
        if !matches!(parts[..at].last(), Some(Protocol::P2p(_))) {
            return Err(CandidateRefusal::NotLiteral);
        }
        let prefix: Multiaddr = parts[..at].iter().cloned().collect();
        self.admits(&prefix, own_listeners)
    }

    /// [`OperatorSet::admits`] for a candidate a DISCOVERY PROVIDER
    /// supplied, which is judged by its own sibling predicate.
    ///
    /// A separate method rather than a flag: rule 6 keeps the
    /// discovered and advertised predicates as distinct names, so that
    /// the day they stop agreeing the divergence has somewhere to go,
    /// and the operator exception is still spelled in this one type.
    ///
    /// # Errors
    /// The class a non-operator candidate was refused for.
    pub fn admits_discovered<'a>(
        &self,
        address: &Multiaddr,
        own_listeners: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), CandidateRefusal> {
        if self.contains(address) {
            return Ok(());
        }
        is_discovered_address(&address.to_string(), own_listeners)
    }

    /// How many operator addresses were refused because the set was full.
    #[must_use]
    pub fn refused_full(&self) -> usize {
        self.read().refused_full
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Inner> {
        self.inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Inner> {
        self.inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    fn addr(s: &str) -> Multiaddr {
        s.parse().expect("valid")
    }

    const ID: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";

    /// ADR-0052 A 2026-09-25, the book's door: a peer's own circuit is
    /// judged by its relay prefix, and every shape the rule refuses is
    /// refused by its class. The non-circuit rows are the control that
    /// the door judges everything else exactly as `admits` does.
    #[test]
    fn a_peers_own_circuit_is_judged_by_its_relay_prefix() {
        const RELAY: &str = "12D3KooWHyNGMf9HTd3Zj6dStdkcc5ycsubW1rEgQSp6k6yfZBoy";
        let me: PeerId = ID.parse().expect("peer");
        let other = PeerId::random();
        let set = OperatorSet::new();
        assert!(set.insert(&addr(&format!("/dns4/relay.example/tcp/4001/p2p/{RELAY}"))));
        let judge = |a: String| set.admits_own_route(&addr(&a), &me, std::iter::empty::<&str>());

        for admitted in [
            format!("/ip4/8.8.8.8/tcp/4001/p2p/{RELAY}/p2p-circuit"),
            format!("/ip4/8.8.8.8/tcp/4001/p2p/{RELAY}/p2p-circuit/p2p/{ID}"),
            // The operator's relay, named: its name resolves.
            format!("/dns4/relay.example/tcp/4001/p2p/{RELAY}/p2p-circuit/p2p/{ID}"),
            // The control: not a circuit, judged as `admits` judges it.
            "/ip4/8.8.8.8/tcp/4001".to_owned(),
        ] {
            assert_eq!(judge(admitted.clone()), Ok(()), "{admitted}");
        }
        for (refused, class) in [
            (
                format!("/ip4/8.8.8.8/tcp/4001/p2p/{RELAY}/p2p-circuit/p2p/{other}"),
                CandidateRefusal::NotOwnCircuit,
            ),
            (
                format!(
                    "/ip4/8.8.8.8/tcp/4001/p2p/{RELAY}/p2p-circuit/p2p/{ID}/p2p-circuit/p2p/{ID}"
                ),
                CandidateRefusal::NotOwnCircuit,
            ),
            (
                format!("/ip4/127.0.0.1/tcp/4001/p2p/{RELAY}/p2p-circuit/p2p/{ID}"),
                CandidateRefusal::SpecialUse,
            ),
            (
                format!("/dns4/a-peers-relay.invalid/tcp/4001/p2p/{RELAY}/p2p-circuit"),
                CandidateRefusal::NotLiteral,
            ),
            // No relay identity: the relay client cannot dial it.
            (
                "/ip4/8.8.8.8/tcp/4001/p2p-circuit".to_owned(),
                CandidateRefusal::NotLiteral,
            ),
            // A stacked prefix is still refused by the shape check.
            (
                format!("/ip4/8.8.8.8/tcp/1/ip4/127.0.0.1/tcp/22/p2p/{RELAY}/p2p-circuit"),
                CandidateRefusal::NotLiteral,
            ),
            (
                "/ip4/127.0.0.1/tcp/4001".to_owned(),
                CandidateRefusal::SpecialUse,
            ),
        ] {
            assert_eq!(judge(refused.clone()), Err(class), "{refused}");
        }
        // THE OPERATOR'S OWN CIRCUIT through a named relay: the set holds
        // the circuit, not the relay's route, and it is admitted whole.
        // The same circuit NOT in the set is the control: refused on its
        // named prefix.
        let named = format!("/dns4/other-relay.example/tcp/4001/p2p/{RELAY}/p2p-circuit/p2p/{ID}");
        assert_eq!(judge(named.clone()), Err(CandidateRefusal::NotLiteral));
        assert!(set.insert(&addr(&named)));
        assert_eq!(
            judge(named),
            Ok(()),
            "the operator's circuit is admitted whole"
        );

        // EVERY OTHER STORE keeps refusing the circuit the book admits.
        assert_eq!(
            set.admits(
                &addr(&format!("/ip4/8.8.8.8/tcp/4001/p2p/{RELAY}/p2p-circuit")),
                std::iter::empty::<&str>()
            ),
            Err(CandidateRefusal::Relayed)
        );
    }

    /// THE PAIR, held together: an operator seed written WITH its
    /// suffix is found when a behaviour offers it BARE, and the reverse.
    /// Two canonicalisations would pass each direction's own test and
    /// fail this one.
    #[test]
    fn one_route_is_one_key_whichever_end_spells_the_suffix() {
        let set = OperatorSet::new();
        assert!(set.insert(&addr(&format!("/dns4/boot.example/tcp/4001/p2p/{ID}"))));
        assert!(set.contains(&addr("/dns4/boot.example/tcp/4001")));

        let other = OperatorSet::new();
        assert!(other.insert(&addr("/ip4/192.168.1.10/tcp/4001")));
        assert!(other.contains(&addr(&format!("/ip4/192.168.1.10/tcp/4001/p2p/{ID}"))));
    }

    /// Rule 9's point: the operator's own name resolves and its LAN seed
    /// routes, where the SAME addresses from a peer are refused.
    #[test]
    fn the_operators_name_and_lan_seed_are_admitted_and_a_peers_are_not() {
        let set = OperatorSet::new();
        let name = addr("/dns4/boot.example/tcp/4001");
        let lan = addr("/ip4/192.168.1.10/tcp/4001");
        let no_listeners: [&str; 0] = [];

        // THE CONTROL FIRST: before the operator's door has seen them,
        // both are refused, so the admissions below are the set's doing.
        assert_eq!(
            set.admits(&name, no_listeners),
            Err(CandidateRefusal::NotLiteral)
        );
        assert_eq!(
            set.admits(&lan, no_listeners),
            Err(CandidateRefusal::PrivateWithoutPrivateListener)
        );

        assert!(set.insert(&name));
        assert!(set.insert(&lan));
        assert_eq!(set.admits(&name, no_listeners), Ok(()));
        assert_eq!(set.admits(&lan, no_listeners), Ok(()));

        // And a DIFFERENT name is still a peer's, and still refused.
        assert_eq!(
            set.admits(&addr("/dns4/a-peers-choice.example/tcp/4001"), no_listeners),
            Err(CandidateRefusal::NotLiteral)
        );
    }

    /// Every clone reads the one set: the operator's door writes through
    /// one handle and a learn site reads through another.
    #[test]
    fn a_clone_sees_what_the_operators_door_wrote() {
        let written = OperatorSet::new();
        let read = written.clone();
        assert!(written.insert(&addr("/dns4/boot.example/tcp/4001")));
        assert!(read.contains(&addr("/dns4/boot.example/tcp/4001")));
    }

    /// Bounded, and a refusal past the bound is counted rather than
    /// evicting an earlier seed.
    #[test]
    fn the_set_stops_at_its_bound_and_counts_what_it_refused() {
        let set = OperatorSet::new();
        for i in 0..MAX_OPERATOR_ADDRESSES {
            let port = 1 + u16::try_from(i).expect("fits");
            assert!(set.insert(&addr(&format!("/ip4/8.8.8.8/tcp/{port}"))));
        }
        assert!(!set.insert(&addr("/ip4/8.8.4.4/tcp/1")), "past the bound");
        assert_eq!(set.refused_full(), 1);
        assert!(
            set.contains(&addr("/ip4/8.8.8.8/tcp/1")),
            "the earliest seed is kept, not evicted"
        );
        assert!(
            set.insert(&addr("/ip4/8.8.8.8/tcp/1")),
            "re-recording a held address succeeds even at the bound"
        );
        assert_eq!(set.refused_full(), 1, "and costs no refusal");
    }
}
